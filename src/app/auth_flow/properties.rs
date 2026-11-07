//! Generated lifecycle tests use only synthetic text. Tasks are constructed but
//! never polled: no greetd socket, PAM, compositor, or account service is used.
use super::*;
use crate::accounts::Account;
use crate::conversation::Conversation;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Creating,
    Ready,
    Submitted,
    Starting,
    Rejected,
    Cancelling,
    Choosing,
    CancelFailed,
}

#[derive(Clone, Debug)]
struct Model {
    stage: Stage,
    input: String,
}

#[derive(Clone, Debug)]
enum Action {
    Prompt(bool),
    Notice(bool),
    Success,
    Reject,
    Type(u16),
    Submit,
    ChangeUser,
    Cancel(bool),
    Select,
    Retry,
    Stale(u8),
}

struct Login;
impl ReferenceStateMachine for Login {
    type State = Model;
    type Transition = Action;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            stage: Stage::Ready,
            input: String::new(),
        })
        .boxed()
    }

    fn transitions(state: &Model) -> BoxedStrategy<Action> {
        let common = prop_oneof![
            any::<u16>().prop_map(Action::Type),
            Just(Action::Submit),
            Just(Action::ChangeUser),
            (0u8..4).prop_map(Action::Stale)
        ];
        let lifecycle = match state.stage {
            Stage::Cancelling => any::<bool>().prop_map(Action::Cancel).boxed(),
            Stage::Choosing => Just(Action::Select).boxed(),
            Stage::CancelFailed => Just(Action::ChangeUser).boxed(),
            Stage::Rejected => Just(Action::Retry).boxed(),
            _ => prop_oneof![
                any::<bool>().prop_map(Action::Prompt),
                any::<bool>().prop_map(Action::Notice),
                Just(Action::Success),
                Just(Action::Reject)
            ]
            .boxed(),
        };
        prop_oneof![common, lifecycle].boxed()
    }

    fn preconditions(model: &Model, action: &Action) -> bool {
        match action {
            Action::Cancel(_) => model.stage == Stage::Cancelling,
            Action::Select => model.stage == Stage::Choosing,
            Action::Retry => model.stage == Stage::Rejected,
            Action::Prompt(_) | Action::Notice(_) | Action::Success | Action::Reject => matches!(
                model.stage,
                Stage::Ready | Stage::Creating | Stage::Submitted | Stage::Starting
            ),
            _ => true,
        }
    }

    fn apply(mut model: Model, action: &Action) -> Model {
        match action {
            Action::Type(value) if model.stage == Stage::Ready => {
                model.input = format!("synthetic-{value}")
            }
            Action::Submit if model.stage == Stage::Ready => {
                model.stage = Stage::Submitted;
                model.input.clear();
            }
            Action::Prompt(_) => {
                model.stage = Stage::Ready;
                model.input.clear();
            }
            Action::Success => {
                model.stage = Stage::Starting;
                model.input.clear();
            }
            Action::Reject => {
                model.stage = if model.stage == Stage::Submitted {
                    Stage::Creating
                } else {
                    Stage::Rejected
                };
                model.input.clear();
            }
            Action::ChangeUser if matches!(model.stage, Stage::Ready | Stage::Rejected) => {
                model.stage = Stage::Cancelling;
                model.input.clear();
            }
            Action::ChangeUser if model.stage == Stage::CancelFailed => {
                model.stage = Stage::Cancelling
            }
            Action::Cancel(ok) => {
                model.stage = if *ok {
                    Stage::Choosing
                } else {
                    Stage::CancelFailed
                }
            }
            Action::Select | Action::Retry => {
                model.stage = Stage::Creating;
                model.input.clear();
            }
            _ => {}
        }
        model
    }
}

struct Harness {
    app: App,
    stale: Attempt,
}

// Do not Debug the App: failures must not dump runtime input or PAM responses.
fn snapshot(app: &App) -> (Phase, String, String, String) {
    (
        app.phase,
        format!("{:?}", app.conversation),
        app.conversation.input().into(),
        app.username.clone(),
    )
}

impl StateMachineTest for Harness {
    type SystemUnderTest = Self;
    type Reference = Login;

    fn init_test(_: &Model) -> Self {
        let mut app = super::super::tests::app();
        app.accounts = ["synthetic-a", "synthetic-b"]
            .map(|name| Account::override_account(name.into(), None))
            .to_vec();
        app.conversation = Conversation::new();
        let stale = app.conversation.attempt();
        let active = app.conversation.begin_attempt();
        app.conversation.receive(
            active,
            conversation::Response::Prompt {
                secret: true,
                message: "Synthetic prompt".into(),
            },
        );
        Self { app, stale }
    }

    fn apply(mut state: Self, _: &Model, action: Action) -> Self {
        let app = &mut state.app;
        let previous_attempt = app.conversation.attempt();
        match action {
            Action::Type(value) => {
                let _ = app.update(Message::InputChanged(format!("synthetic-{value}")));
            }
            Action::Submit => {
                // Exercise the real conversation submission and transition
                // function without constructing a live transport client.
                if let Some((attempt, response)) = app.conversation.submit() {
                    assert!(app.conversation.accepts(attempt));
                    assert!(response.starts_with("synthetic-") || response.is_empty());
                    app.phase = Phase::Authenticating;
                    assert!(app.conversation.input().is_empty());
                }
            }
            Action::Prompt(secret) | Action::Notice(secret) => {
                let prompt = matches!(action, Action::Prompt(_));
                let response = if prompt {
                    auth::Response::Prompt {
                        secret,
                        message: "Synthetic prompt:".into(),
                    }
                } else {
                    auth::Response::Message {
                        error: secret,
                        message: "Synthetic notice".into(),
                    }
                };
                let active = app.conversation.attempt();
                let change = transition(
                    &mut app.conversation,
                    app.phase,
                    active,
                    response,
                    app.selected_session.as_ref(),
                )
                .unwrap();
                let effect = app.apply_auth_transition(change);
                if prompt {
                    assert!(
                        matches!(effect, AuthEffect::Prompt { secret: actual, .. } if actual == secret)
                    );
                } else {
                    assert!(
                        matches!(effect, AuthEffect::Acknowledge { error, .. } if error == secret)
                    );
                }
            }
            Action::Success => {
                let active = app.conversation.attempt();
                let change = transition(
                    &mut app.conversation,
                    app.phase,
                    active,
                    auth::Response::Success,
                    app.selected_session.as_ref(),
                )
                .unwrap();
                let effect = app.apply_auth_transition(change);
                assert!(matches!(
                    effect,
                    AuthEffect::StartSession(_) | AuthEffect::Exit
                ));
            }
            Action::Reject => {
                let old = app.conversation.attempt();
                let submitted = app.phase == Phase::Authenticating;
                let _ = app.handle_auth_result(
                    old,
                    Ok((
                        None,
                        auth::Response::Error {
                            authentication: true,
                            description: "Synthetic rejection".into(),
                        },
                    )),
                );
                assert!(app.conversation.input().is_empty());
                if submitted {
                    assert!(!app.conversation.accepts(old));
                    assert_eq!(app.phase, Phase::CreatingSession);
                } else {
                    assert_eq!(app.phase, Phase::Failed);
                }
            }
            Action::ChangeUser => {
                let _ = if app.phase == Phase::UserSelectionCancellationFailed {
                    app.update(Message::RetryUserSelectionCancellation)
                } else {
                    app.update(Message::ChangeUser)
                };
            }
            Action::Cancel(ok) => {
                let attempt = app.conversation.attempt();
                let _ = app.update(Message::UserSelectionCancelled {
                    attempt,
                    result: if ok {
                        Ok(())
                    } else {
                        Err("Synthetic cancel failure".into())
                    },
                });
            }
            Action::Select => {
                let _ = app.update(Message::SelectAccount(app.accounts[0].clone()));
            }
            Action::Retry => {
                let _ = app.update(Message::Retry);
            }
            Action::Stale(kind) => {
                let before = snapshot(app);
                let response = match kind {
                    0 => auth::Response::Success,
                    1 => auth::Response::Prompt {
                        secret: false,
                        message: "Synthetic stale prompt".into(),
                    },
                    2 => auth::Response::Message {
                        error: true,
                        message: "Synthetic stale notice".into(),
                    },
                    _ => auth::Response::Error {
                        authentication: true,
                        description: "Synthetic stale rejection".into(),
                    },
                };
                let _ = app.handle_auth_result(state.stale, Ok((None, response)));
                assert!(
                    snapshot(app) == before,
                    "stale response mutated state (values redacted)"
                );
            }
        }
        if !app.conversation.accepts(previous_attempt) {
            state.stale = previous_attempt;
        }
        state
    }

    fn check_invariants(state: &Self, model: &Model) {
        let app = &state.app;
        let expected = match model.stage {
            Stage::Ready => Phase::WaitingForInput,
            Stage::Creating => Phase::CreatingSession,
            Stage::Submitted => Phase::Authenticating,
            Stage::Starting => Phase::StartingSession,
            Stage::Rejected => Phase::Failed,
            Stage::Cancelling => Phase::CancellingForUserSelection,
            Stage::Choosing => Phase::SelectingUser,
            Stage::CancelFailed => Phase::UserSelectionCancellationFailed,
        };
        assert_eq!(app.phase, expected);
        assert!(
            app.conversation.input() == model.input,
            "response mismatch (redacted)"
        );
        if model.stage != Stage::Ready {
            assert!(app.conversation.input().is_empty());
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn generated_login_lifecycle(sequential 1..80 => Harness);
}
