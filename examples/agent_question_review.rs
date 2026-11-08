//! Offline Agent question cards: the real card widget and question store with
//! fixture cards. This process never signs in or sends anything: an answer a
//! click produces is recorded for the review script instead of being sent.
pub use makepad_widgets;
use hagency::agent_questions::{AnswerValue, Card, CardOption, CardQuestion, CardState};
use makepad_widgets::*;
use rinx::agent_chat::question_card::AgentQuestionCardWidgetRefExt;
use rinx::hagency::pending_questions::{
    self, PendingQuestionsChanged, QuestionAnswerRequested, QuestionAnswerSent,
};
use std::collections::BTreeMap;

app_main!(App);
fn review_size() -> Vec2d {
    Vec2d {
        x: std::env::var("WIZARD_REVIEW_WIDTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(720.0),
        y: std::env::var("WIZARD_REVIEW_HEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1180.0),
    }
}
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {main_window := Window {
            window.inner_size: #(review_size())
            window.title: "Hagency · Agent question review"
            body +: {
                page := SolidView {
                    width: Fill height: Fill
                    draw_bg.color: mod.widgets.RINX_PAGE
                    cards := ScrollYView {
                        width: Fill height: Fill flow: Down padding: 20 spacing: 16
                        single := mod.widgets.AgentQuestionCard {}
                        several := mod.widgets.AgentQuestionCard {}
                        answered := mod.widgets.AgentQuestionCard {}
                        expired := mod.widgets.AgentQuestionCard {}
                    }
                }
            }
        }}
    }
}

const AGENT: &str = "@_hagency_agt_review:example.invalid";
/// The four cards, by widget slot: (slot, card event, card id digit).
const SLOTS: [(&str, &str, char); 4] = [
    ("single", "$card_single", 'a'),
    ("several", "$card_several", 'b'),
    ("answered", "$card_answered", 'c'),
    ("expired", "$card_expired", 'd'),
];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
fn option(label: &str, description: &str) -> CardOption {
    CardOption {
        label: label.into(),
        description: description.into(),
    }
}
fn card(digit: char, trigger: &str, questions: Vec<CardQuestion>, withheld: u64) -> Card {
    Card {
        version: 1,
        id: format!("aq_{}", digit.to_string().repeat(64)),
        trigger_event_id: trigger.into(),
        thread_root: trigger.into(),
        expires_at_ms: now_ms() + 600_000,
        questions,
        responders: Vec::new(),
        withheld,
        state: CardState::Pending,
        answered_by: None,
        answers: BTreeMap::new(),
    }
}
fn database() -> CardQuestion {
    CardQuestion {
        id: "db".into(),
        header: "Database".into(),
        question: "Which database should the new service use?".into(),
        options: vec![
            option(
                "Postgres (Recommended)",
                "Durable and familiar to the team.",
            ),
            option("SQLite", "Simplest to run on one machine."),
        ],
        other: true,
        multi_select: false,
    }
}
fn fixtures() {
    pending_questions::review_identity("@alice:example.invalid");
    let several = card(
        'b',
        "$trigger_several",
        vec![
            CardQuestion {
                id: "lang".into(),
                header: "语言".into(),
                question: "用哪种语言重写这个模块？".into(),
                options: vec![
                    option("Rust", "与现有代码一致。"),
                    option("Go", "团队更熟悉。"),
                    option("Python", ""),
                ],
                other: false,
                multi_select: false,
            },
            CardQuestion {
                id: "cadence".into(),
                header: "Cadence".into(),
                question: "How often should the report run?".into(),
                options: Vec::new(),
                other: true,
                multi_select: false,
            },
        ],
        1,
    );
    let answered = card('c', "$trigger_answered", vec![database()], 0);
    let expired = card('d', "$trigger_expired", vec![database()], 0);
    pending_questions::posted(
        "!room:example.invalid",
        "$card_single",
        AGENT,
        "小黄瓜",
        now_ms(),
        card('a', "$trigger_single", vec![database()], 0),
    );
    pending_questions::posted(
        "!room:example.invalid",
        "$card_several",
        AGENT,
        "Atlas",
        now_ms(),
        several,
    );
    pending_questions::posted(
        "!room:example.invalid",
        "$card_answered",
        AGENT,
        "Atlas",
        now_ms(),
        answered.clone(),
    );
    pending_questions::posted(
        "!room:example.invalid",
        "$card_expired",
        AGENT,
        "Atlas",
        now_ms(),
        expired.clone(),
    );
    pending_questions::edited(
        "!room:example.invalid",
        "$card_answered",
        AGENT,
        answered.answered(
            "@alice:example.invalid",
            BTreeMap::from([("db".into(), AnswerValue::Text("CockroachDB".into()))]),
        ),
    );
    pending_questions::edited(
        "!room:example.invalid",
        "$card_expired",
        AGENT,
        expired.expired(),
    );
}

fn scoped_fixture(other_member: bool) {
    pending_questions::review_identity(if other_member {"@other:example.invalid"}else{"@alice:example.invalid"});
    let questions=(0..4).map(|index| CardQuestion {
        id:format!("q{index}"),header:String::new(),question:format!("Native question {index}?"),
        options:vec![option(&format!("Q{index} A"),""),option(&format!("Q{index} B"),"")],
        other:false,multi_select:index==0,
    }).collect();
    let mut card=card('e',"$trigger_single",questions,0);
    card.version=hagency::agent_questions::SCOPED_VERSION;
    card.responders=vec!["@alice:example.invalid".into()];
    pending_questions::posted("!room:example.invalid","$card_single",AGENT,"小黄瓜",now_ms(),card);
}

#[derive(Script, ScriptHook)]
struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    requests: Vec<serde_json::Value>,
    /// Slots drawn now; a review window is shorter than all four cards.
    #[rust]
    shown: Vec<String>,
}
impl App {
    fn render(&mut self, cx: &mut Cx) {
        for (slot, event, _) in SLOTS {
            let view = pending_questions::get(event)
                .filter(|_| self.shown.iter().any(|s| s == slot))
                .map(|c| pending_questions::view(&c, now_ms()));
            self.ui
                .agent_question_card(cx, &[LiveId::from_str(slot)])
                .set_view(cx, view.as_ref());
        }
        self.ui.redraw(cx);
    }
    fn report(&self) {
        let cards: serde_json::Map<String, serde_json::Value> = SLOTS
            .iter()
            .filter_map(|(slot, event, _)| {
                let c = pending_questions::get(event)?;
                Some((
                    (*slot).to_owned(),
                    serde_json::json!({"local": format!("{:?}", c.local), "choices": c.choices, "texts": c.texts,
                        "state": c.card.state, "ready": c.ready()}),
                ))
            })
            .collect();
        let report = serde_json::json!({"requests": self.requests, "cards": cards});
        let path = std::path::PathBuf::from(std::env::var("HAGENCY_DESKTOP_DATA_DIR").unwrap())
            .join("question-review.json");
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        rinx::theme::init_standalone(vm);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        rinx::app::register_widgets(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::Custom(command) = event {
            if matches!(command.as_str(), "theme:dark" | "theme:light") {
                rinx::theme::select(
                    cx,
                    rinx::theme::Selection {
                        appearance: if command == "theme:dark" {
                            rinx::theme::Appearance::Dark
                        } else {
                            rinx::theme::Appearance::Light
                        },
                        accent: rinx::theme::Accent::Teal,
                    },
                )
                .unwrap();
            }
            match command.as_str() {
                "question:ready" => {
                    fixtures();
                    self.requests.clear();
                    self.shown = vec!["single".into(), "several".into()];
                    self.render(cx);
                }
                "question:multi" | "question:scoped-other" => {
                    scoped_fixture(command == "question:scoped-other");
                    self.requests.clear();
                    self.shown=vec!["single".into()];
                    self.render(cx);
                }
                "question:inspect" => self.report(),
                "question:sent-ok" | "question:sent-fail" => {
                    if let Some(card_id) = self.requests.last().and_then(|r| r["card"].as_str()) {
                        let result = if command == "question:sent-ok" {
                            Ok(())
                        } else {
                            Err("answer_send_failed".to_owned())
                        };
                        let generation = self
                            .requests
                            .last()
                            .and_then(|r| r["generation"].as_u64())
                            .unwrap();
                        pending_questions::sent(&QuestionAnswerSent {
                            generation,
                            key: card_id.to_owned(),
                            result,
                        });
                    }
                    self.render(cx);
                }
                "question:agent-answered" => {
                    // The Agent's own edit after another member answered first.
                    if let Some(c) = pending_questions::get("$card_single") {
                        let settled = c.card.answered(
                            "@bob:example.invalid",
                            BTreeMap::from([("db".into(), AnswerValue::Option("SQLite".into()))]),
                        );
                        pending_questions::edited(
                            "!room:example.invalid",
                            "$card_single",
                            AGENT,
                            settled,
                        );
                    }
                    self.render(cx);
                }
                _ => {
                    if let Some(slots) = command.strip_prefix("question:show:") {
                        self.shown = slots.split('+').map(str::to_owned).collect();
                        self.render(cx);
                    }
                }
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        if let Event::Actions(actions) = event {
            let mut changed = false;
            for action in actions {
                if let Some(request) = action.downcast_ref::<QuestionAnswerRequested>() {
                    // Recorded, never sent.
                    self.requests.push(serde_json::json!({"room": request.room_id, "card": request.key, "generation": request.generation,
                        "agent": request.agent, "content": request.content}));
                }
                changed |= action.downcast_ref::<PendingQuestionsChanged>().is_some();
            }
            if changed {
                self.render(cx);
            }
        }
    }
}
