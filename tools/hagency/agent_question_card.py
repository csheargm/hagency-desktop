#!/usr/bin/env python3
"""Agent question cards in hidden, isolated Makepad windows (headless instrument).

Drives the offline `agent_question_review` example through MAKEPAD_REMOTE
(/snap, /click, /t, /event, /g). No Matrix client, homeserver, Agent or model
is started, and no answer is sent: the example records the exact answer event
content a click produces, and this script checks it. Build first:

    cargo build --release --features onboarding-test-support --example agent_question_review

Writes one PNG and one widget snapshot per step, and report.json, per case.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CASES = {
    'en-desktop': ('en', 720, 1180, False),
    'zh-desktop': ('zh-CN', 720, 1180, False),
    'en-dark': ('en', 720, 1180, True),
    'zh-compact': ('zh-CN', 390, 1500, False),
}
WORDS = {
    'en': dict(asks='小黄瓜 asks', waiting='Waiting for an answer', sending='Sending…',
               failed='Not sent · try again', sent='Sent · waiting for the Agent',
               answered='Answered by @alice:example.invalid', bob='Answered by @bob:example.invalid',
               expired='Expired · the Agent continued on its own', send='Send answer'),
    'zh-CN': dict(asks='小黄瓜 提问', waiting='等待回答', sending='正在发送…',
                  failed='未发送 · 请重试', sent='已发送 · 等待 Agent',
                  answered='已由 @alice:example.invalid 回答', bob='已由 @bob:example.invalid 回答',
                  expired='已过期 · Agent 已自行继续', send='发送回答'),
}
CARD = 'aq_' + 'a' * 64
SEVERAL = 'aq_' + 'b' * 64


class Review:
    def __init__(self, binary, output, case):
        self.language, self.width, self.height, dark = case
        self.words = WORDS[self.language]
        self.output = output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.profile = self.output / 'profile'
        self.profile.mkdir(exist_ok=True)
        (self.profile / 'ui-language.json').write_text(json.dumps(self.language))
        self.checks = []
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            self.port = sock.getsockname()[1]
        env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(self.profile), RINX_DATA_DIR=str(self.profile),
                   MAKEPAD_REMOTE=str(self.port), MAKEPAD_HIDE_WINDOWS='1', MAKEPAD_NO_FOCUS='1',
                   WIZARD_REVIEW_WIDTH=str(self.width), WIZARD_REVIEW_HEIGHT=str(self.height))
        env.pop('MAKEPAD_FOCUS', None)
        self.log = (self.output / 'native.log').open('w')
        self.process = subprocess.Popen([str(binary.resolve())], cwd=ROOT, env=env,
                                        stdout=self.log, stderr=subprocess.STDOUT)
        for _ in range(150):
            if self.process.poll() is not None:
                raise RuntimeError('Offline review exited; see native.log')
            try:
                if self.request('/s')['pid'] == self.process.pid:
                    break
            except OSError:
                time.sleep(.2)
        else:
            raise RuntimeError('Review window did not become ready')
        self.event('theme:dark' if dark else 'theme:light')

    def request(self, path, **params):
        url = f'http://127.0.0.1:{self.port}{path}?' + urllib.parse.urlencode(params)
        with urllib.request.urlopen(url, timeout=15) as response:
            return json.load(response)

    def event(self, data):
        self.request('/event', data=data)
        time.sleep(.3)

    def snap(self):
        return [w for w in self.request('/snap', w=0)['s'] if w.get('r', [0, 0, 0, 0])[2] > 0]

    def reveal(self, name):
        """Scroll the card list until the whole card is inside its viewport."""
        for _ in range(30):
            snap = self.snap()
            view = next(w['r'] for w in snap if w.get('i') == 'cards')
            card = next((w['r'] for w in snap if w.get('i') == name), None)
            if card is None:
                dy = 300
            elif card[1] < view[1]:
                dy = card[1] - view[1] - 10
            elif card[1] + card[3] > view[1] + view[3]:
                dy = min(card[1] + card[3] - (view[1] + view[3]) + 10, card[1] - view[1] - 10)
            else:
                return snap, card
            x, y, w, h = view
            self.request('/m', w=0, k='scroll', x=x + w / 2, y=y + h / 2, dy=dy, precise=1, wait=1)
            time.sleep(.3)
        raise AssertionError(('not reachable in the card list', name))

    def card(self, name):
        snap, rect = self.reveal(name)
        return [w for w in snap if inside(w['r'], rect)]

    def texts(self, name):
        return [w.get('t', '') for w in self.card(name)]

    def button(self, name, text):
        return next(w for w in self.card(name) if w.get('ty') == 'Button' and w.get('t') in (text, '✓ ' + text))

    def click(self, widget):
        x, y, width, height = widget['r']
        self.request('/click', w=0, x=x + width / 2, y=y + height / 2, wait=1)
        time.sleep(.4)

    def type(self, text):
        self.request('/t', w=0, t=text, wait=1)
        time.sleep(.3)

    def inspect(self):
        self.event('question:inspect')
        return json.loads((self.profile / 'question-review.json').read_text())

    def status(self, name):
        return next(w['t'] for w in self.card(name) if w.get('i') == 'status_label')

    def inside_window(self):
        for x in self.snap():
            x0, _, width, _ = x['r']
            assert x0 >= 0 and x0 + width <= self.width + 1, ('inside the window', x)

    def capture(self, name):
        for attempt in range(3):
            try:
                image = self.request('/g', w=0, scale=1)
                assert image.get('capture_kind') == 'pixels'
                shutil.copy2(image['png'], self.output / (name + '.png'))
                (self.output / (name + '.json')).write_text(json.dumps(self.snap(), ensure_ascii=False, indent=2))
                return
            except urllib.error.HTTPError as error:
                if error.code != 404 or attempt == 2:
                    raise
                time.sleep(.3)

    def run(self):
        w = self.words
        self.event('question:ready')
        # Every state is drawn: open single and multi-question cards, answered, expired.
        single = self.texts('single')
        assert w['asks'] in single and w['waiting'] in single, single
        assert 'Which database should the new service use?' in single
        for label in ('Postgres (Recommended)', 'SQLite'):
            assert self.button('single', label).get('enabled', True), label
        several = self.texts('several')
        assert '用哪种语言重写这个模块？' in several and 'How often should the report run?' in several, several
        assert any('private question' in t or '私密问题' in t for t in several), 'withheld notice'
        inputs = [x for x in self.snap() if x.get('i') == 'other_input']
        assert len(inputs) == 2, 'Other inputs only on open cards that take text'
        sends = [x for x in self.snap() if x.get('ty') == 'Button' and x.get('t') == w['send']]
        assert len(sends) == 2 and not any(x.get('enabled', True) for x in sends), 'Send waits for an answer'
        self.inside_window()
        self.capture('cards-open')
        self.event('question:show:answered+expired')
        answered = self.texts('answered')
        assert w['answered'] in answered and '→ CockroachDB' in answered, answered
        assert not any(x.get('ty') == 'Button' for x in self.card('answered')), 'a settled card has no buttons'
        assert not any(x.get('i') == 'other_input' for x in self.card('answered'))
        expired = self.texts('expired')
        assert w['expired'] in expired, expired
        # An expired card keeps its choices for the record, all disabled, with no input or Send.
        buttons = [x for x in self.card('expired') if x.get('ty') == 'Button']
        assert [x['t'] for x in buttons] == ['Postgres (Recommended)', 'SQLite'], buttons
        assert not any(x.get('enabled', True) for x in buttons)
        assert not any(x.get('i') == 'other_input' for x in self.card('expired'))
        self.inside_window()
        self.capture('cards-settled')
        self.event('question:show:single+several')
        self.checks.append('open, multi-question, answered and expired cards; Other inputs; Send disabled until answered')

        # One question: the option click is the answer. A failed send can be retried.
        self.click(self.button('single', 'SQLite'))
        report = self.inspect()
        assert len(report['requests']) == 1, report
        content = report['requests'][0]['content']
        assert content == {'version': 1, 'cardId': CARD, 'answers': {'db': {'option': 'SQLite'}},
                           'm.relates_to': {'rel_type': 'm.reference', 'event_id': '$card_single'}}, content
        assert self.status('single') == w['sending']
        assert not self.button('single', 'Postgres (Recommended)').get('enabled', True), 'no second answer while sending'
        self.capture('single-sending')
        self.event('question:sent-fail')
        assert self.status('single') == w['failed']
        self.click(self.button('single', 'Postgres (Recommended)'))
        report = self.inspect()
        assert len(report['requests']) == 2
        assert report['requests'][1]['content']['answers'] == {'db': {'option': 'Postgres (Recommended)'}}
        self.event('question:sent-ok')
        assert self.status('single') == w['sent']
        self.capture('single-sent')
        self.checks.append('one-question click sends the exact answer event; failed send retries; sent state shown')

        # Several questions: choices mark, typed text completes, Send sends all.
        self.click(self.button('several', 'Go'))
        assert len(self.inspect()['requests']) == 2, 'a choice alone is not an answer'
        assert self.button('several', 'Go')['t'] == '✓ Go'
        send = next(x for x in self.card('several') if x.get('ty') == 'Button' and x.get('t') == w['send'])
        assert not send.get('enabled', True)
        self.click(next(x for x in self.card('several') if x.get('i') == 'other_input'))
        self.type('Every Monday')
        send = next(x for x in self.card('several') if x.get('ty') == 'Button' and x.get('t') == w['send'])
        assert send.get('enabled', True), 'Send enabled once every question has an answer'
        self.capture('several-ready')
        self.click(send)
        report = self.inspect()
        assert len(report['requests']) == 3, report
        assert report['requests'][2]['content']['answers'] == {'lang': {'option': 'Go'}, 'cadence': {'text': 'Every Monday'}}
        assert report['requests'][2]['content']['m.relates_to']['event_id'] == '$card_several'
        assert self.status('several') == w['sending']
        self.checks.append('multi-question card: choices mark, typed text, Send sends both answers once')

        # Another member answered first: the Agent's edit settles the card.
        self.event('question:show:single')
        self.event('question:agent-answered')
        single = self.texts('single')
        assert w['bob'] in single and '→ SQLite' in single, single
        assert not any(x.get('ty') == 'Button' for x in self.card('single'))
        self.capture('single-answered-elsewhere')
        self.checks.append("the Agent's answered edit replaces the buttons with the answer")
        self.event('question:multi')
        assert 'Native question 3?' in self.texts('single'), 'the fourth native question is visible'
        for label in ('Q0 A', 'Q0 B', 'Q0 A', 'Q1 A', 'Q2 B', 'Q3 B'):
            self.click(self.button('single', label))
        assert not self.inspect()['requests'], 'multi-select waits for the explicit Send button'
        assert self.button('single', 'Q0 B')['t'] == '✓ Q0 B'
        assert self.button('single', 'Q0 A')['t'] == 'Q0 A'
        send = self.button('single', w['send'])
        assert send.get('enabled', True)
        self.inside_window()
        self.capture('native-four-multiselect')
        self.click(send)
        answer = self.inspect()['requests'][0]['content']
        assert answer['version'] == 2
        assert answer['answers'] == {'q0': {'options': ['Q0 B']}, 'q1': {'option': 'Q1 A'},
                                      'q2': {'option': 'Q2 B'}, 'q3': {'option': 'Q3 B'}}
        self.checks.append('four native questions: multi-select toggles independently and Send returns exact arrays')
        self.event('question:scoped-other')
        assert '@alice:example.invalid' in '\n'.join(self.texts('single'))
        assert not self.button('single', 'Q0 A').get('enabled', True)
        self.click(self.button('single', 'Q0 A'))
        assert not self.inspect()['requests'], 'a different joined member cannot answer'
        self.capture('native-scoped-readonly')
        self.checks.append('question audience is displayed and other room members cannot submit')
        # A script error in a widget definition silently drops it from the app.
        log = (self.output / 'native.log').read_text()
        errors = [line for line in log.splitlines() if line.startswith('[E] ') or 'panicked at' in line]
        assert not errors, errors[:5]

    def close(self, passed):
        if not passed:
            try:
                self.capture('failure')
            except Exception:
                pass
        (self.output / 'report.json').write_text(json.dumps(
            {'passed': passed, 'renderer': 'hidden native Makepad window', 'checks': self.checks}, indent=2))
        try:
            self.request('/quit')
        except OSError:
            pass
        try:
            self.process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=8)
        self.log.close()


def inside(rect, outer):
    x, y, w, h = rect
    ox, oy, ow, oh = outer
    return x >= ox - 1 and y >= oy - 1 and x + w <= ox + ow + 1 and y + h <= oy + oh + 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/examples/agent_question_review')
    parser.add_argument('--output', type=Path, default=ROOT / '.run/agent-question-review')
    parser.add_argument('--case', choices=list(CASES), action='append')
    args = parser.parse_args()
    results = {}
    for name in args.case or CASES:
        review = Review(args.binary, args.output / name, CASES[name])
        passed = False
        try:
            review.run()
            passed = True
            print(name, 'PASS', len(review.checks), flush=True)
        finally:
            review.close(passed)
            results[name] = passed
    print(json.dumps(results))


if __name__ == '__main__':
    main()
