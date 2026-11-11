#!/usr/bin/env python3
"""Check docked dividers and native webview resizing in hidden native windows."""
import argparse
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from PIL import Image, ImageChops
from member_invite_review import InviteReview


class Review(InviteReview):
    def wait_widget(self, ident):
        for _ in range(50):
            try:
                return self.widget(ident)
            except StopIteration:
                time.sleep(.1)
        raise AssertionError(f'{ident} did not render')

    def move(self, x, y):
        self.request('/m', w=0, k='move', x=x, y=y, wait=1)
        time.sleep(.2)

    def crop(self, name, x, y):
        image = Image.open(self.output / (name + '.png')).convert('RGB')
        scale = image.width / self.width
        return image.crop(tuple(round(value * scale) for value in (x - 2, y - 10, x + 2, y + 10)))

    def changes(self, before, after, x, y):
        return ImageChops.difference(self.crop(before, x, y), self.crop(after, x, y)).getbbox()

    def interrupted_transition(self, prefix, left_x, right_x, y):
        # A native child view or overlay can consume the move between edges.
        # The regular remote /m down injects a move first, so ask the fixture
        # to send the down on its own and release it through the native bridge.
        for source, target, name in ((left_x, right_x, 'right'), (right_x, left_x, 'left')):
            self.move(source, y)
            self.request('/event', data=f'rooms:down-without-move:{target}:{y}')
            time.sleep(.3)
            self.capture(f'{prefix}-{name}-down')
            self.request('/m', w=0, k='up', x=target, y=y, b=0, wait=1)
            time.sleep(.3)
            self.capture(f'{prefix}-{name}-up')
            assert not self.changes(prefix + '-idle', f'{prefix}-{name}-up', source, y), (prefix, name, 'old grip remained')
            assert self.changes(prefix + '-idle', f'{prefix}-{name}-up', target, y), (prefix, name, 'new grip missing')

    def run(self, web_url):
        # The room list is created lazily; select only after its first draw.
        self.wait_widget('rooms_list')
        self.request('/event', data='rooms:open-chat')
        self.wait_widget('restore_status_view')
        self.request('/event', data='rooms:open-apps')
        time.sleep(.4)
        handle = self.widget('handle')['r']
        dock = self.widget('dock')['r']
        chat = self.widget('restore_status_view')['r']
        assert handle[2] == 4, handle
        left_x, center_y = chat[0] - 2, dock[1] + dock[3] / 2
        right_x = handle[0] + handle[2] / 2
        self.move(chat[0] + 100, center_y - 140)
        self.capture('catalog-idle')
        self.move(left_x, center_y)
        self.capture('left-hover')
        assert self.changes('catalog-idle', 'left-hover', left_x, center_y)
        self.move(right_x, center_y)
        self.capture('catalog-hover')
        assert self.changes('catalog-idle', 'catalog-hover', right_x, center_y)
        assert not self.changes('catalog-idle', 'catalog-hover', left_x, center_y), 'Left hover must clear'
        diff = ImageChops.difference(self.crop('left-hover', left_x, center_y), self.crop('catalog-hover', right_x, center_y))
        assert max(high for _, high in diff.getextrema()) <= 2, ('Splitters differ', diff.getextrema())
        self.interrupted_transition('catalog', left_x, right_x, center_y)
        self.checks.append('Only the active divider has a grip, including when an intervening pointer move is missed')

        self.drag(handle, center_y, 60)
        width = self.widget('mini_apps_side')['r'][2]
        self.request('/event', data='theme:dark' if self.language == 'zh-CN' else 'theme:light')
        time.sleep(.3)
        assert abs(self.widget('mini_apps_side')['r'][2] - width) < 1
        self.widget('restore_status_view')
        self.capture('retained-width')
        self.checks.append('Native hit margin resizes the mini-app edge; width survives theme reapply')

        self.request('/event', data='rooms:open-web:' + web_url)
        time.sleep(1)
        assert self.widget('native_page')['r'][2] > 0
        handle = self.widget('handle')['r']
        right_x = handle[0] + handle[2] / 2
        self.move(right_x - 80, center_y - 140)
        self.capture('web-idle')
        self.move(right_x, center_y)
        self.capture('web-hover')
        assert self.changes('web-idle', 'web-hover', right_x, center_y)
        self.interrupted_transition('web', left_x, right_x, center_y)
        self.drag(handle, center_y, 40)
        self.capture('web-resized')
        assert self.widget('native_page')['r'][2] > 0
        self.drag_rooms(left_x, center_y, 40)
        self.checks.append('Local native webview stays open as both divider edges resize')
        log = (self.output / 'native.log').read_text()
        assert 'variable __script_source__ not found' not in log, 'Navigation icon tried to style a missing widget'

    def drag(self, handle, y, distance):
        before = self.widget('mini_apps_side')['r'][2]
        # Begin in the native hit margin, outside the painted four-point strip.
        start = handle[0] - 2
        self.request('/m', w=0, k='down', x=start, y=y, b=0, wait=1)
        for index in range(1, 9):
            self.request('/m', w=0, k='move', x=start - distance * index / 8, y=y, wait=1)
        self.request('/m', w=0, k='up', x=start - distance, y=y, b=0, wait=1)
        time.sleep(.3)
        after = self.widget('mini_apps_side')['r'][2]
        assert abs(after - before - distance) < 1, (before, after)

    def drag_rooms(self, x, y, distance):
        before = self.widget('restore_status_view')['r'][0]
        self.request('/m', w=0, k='down', x=x, y=y, b=0, wait=1)
        for index in range(1, 9):
            self.request('/m', w=0, k='move', x=x - distance * index / 8, y=y, wait=1)
        self.request('/m', w=0, k='up', x=x - distance, y=y, b=0, wait=1)
        time.sleep(.3)
        after = self.widget('restore_status_view')['r'][0]
        assert abs(after - before + distance) < 1, (before, after)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()

    requests = []
    class LocalPage(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            requests.append(self.path)
            body = b'<html><body><h1>Local splitter fixture</h1></body></html>'
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), LocalPage)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for language, dark in [('zh-CN', False), ('en', True)]:
            output = args.output / language
            os.environ['RINX_DATA_DIR'] = str((output / 'profile').resolve())
            review = Review(args.binary, output, (language, 1440, 850, dark, False))
            ok = False
            try:
                review.run(f'http://127.0.0.1:{server.server_port}/')
                ok = True
                print(language, 'PASS', len(review.checks), flush=True)
            finally:
                review.close(ok)
        assert requests, 'The isolated webview must load the local fixture'
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
