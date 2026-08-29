use std::time::Instant;

use chrono::{Local, TimeZone};
use clap::ValueEnum;
use iced::widget::text_input;
use iced::Task;

use crate::accounts::Account;
use crate::power::Action as PowerAction;
use crate::sessions::Session;

use super::auth_flow::{Attempt, Phase};
use super::{App, Message, PowerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Fixture {
    Selected,
    Users,
    DuplicateNames,
    LargeAccountSet,
    VisiblePrompt,
    InformationalMessage,
    CancellationProgress,
    CancellationFailure,
    AuthenticationFailure,
    DiscoveryFailure,
    SessionFailure,
    PowerFailure,
    PowerConfirmation,
}

struct State {
    accounts: Vec<Account>,
    selected: Option<Account>,
    prompt: String,
    message: String,
    message_is_error: bool,
    secret: bool,
    phase: Phase,
    session: Option<Session>,
    power_state: PowerState,
}

pub(super) fn build(
    fixture: Fixture,
    username: Option<String>,
    display_name: Option<String>,
) -> (App, Task<Message>) {
    let input_id = text_input::Id::new("authentication-input");
    let state = State::new(fixture, username, display_name);
    let selected_session = state.session.clone();
    let sessions = state.session.into_iter().collect();
    let (username, display_name) = state
        .selected
        .as_ref()
        .map(|account| (account.username.clone(), account.display_name.clone()))
        .unwrap_or_else(|| (String::new(), "Select a user".into()));
    let focus_input = state.phase == Phase::WaitingForInput;
    let app = App {
        username,
        display_name,
        accounts: state.accounts,
        input: String::new(),
        input_id: input_id.clone(),
        prompt: state.prompt,
        message: Some(state.message),
        message_is_error: state.message_is_error,
        secret: state.secret,
        phase: state.phase,
        client: None,
        sessions,
        selected_session,
        started_at: Instant::now(),
        now: preview_now(),
        power_state: state.power_state,
        attempt: Attempt::initial(),
        selection_session_cancelled: false,
        closing: None,
        preview: true,
    };
    let task = if focus_input {
        text_input::focus(input_id)
    } else {
        Task::none()
    };
    (app, task)
}

impl State {
    fn new(fixture: Fixture, username: Option<String>, display_name: Option<String>) -> Self {
        let synthetic_identity = username.is_none();
        let username = username.unwrap_or_else(|| "preview".into());
        let selected = Account::override_account(
            username,
            display_name.or_else(|| synthetic_identity.then(|| "Preview User".into())),
        );
        let session = preview_session();
        let message = "Preview mode: credentials and power actions are simulated".to_owned();
        let mut state = Self {
            accounts: vec![selected.clone()],
            selected: Some(selected),
            prompt: "Password".into(),
            message,
            message_is_error: false,
            secret: true,
            phase: Phase::WaitingForInput,
            session: Some(session),
            power_state: PowerState::Idle,
        };

        match fixture {
            Fixture::Selected => {}
            Fixture::Users => state.select_accounts(accounts([("alice", "Alice"), ("bob", "Bob")])),
            Fixture::DuplicateNames => state.select_accounts(accounts([
                ("alex", "Alex Morgan"),
                ("amorgan", "Alex Morgan"),
            ])),
            Fixture::LargeAccountSet => state.select_accounts(
                (1..=24)
                    .map(|number| {
                        Account::override_account(
                            format!("user{number:02}"),
                            Some(format!("Preview User {number:02}")),
                        )
                    })
                    .collect(),
            ),
            Fixture::VisiblePrompt => {
                state.prompt = "Verification code".into();
                state.secret = false;
            }
            Fixture::InformationalMessage => {
                state.message = "Touch the security key, then enter your password".into();
            }
            Fixture::CancellationProgress => {
                state.select_accounts(accounts([("alice", "Alice"), ("bob", "Bob")]));
                state.phase = Phase::CancellingForUserSelection;
                state.message = "Still changing user…".into();
            }
            Fixture::CancellationFailure => {
                state.select_accounts(accounts([("alice", "Alice"), ("bob", "Bob")]));
                state.phase = Phase::UserSelectionCancellationFailed;
                state.message = "Could not cancel the previous login attempt".into();
                state.message_is_error = true;
            }
            Fixture::AuthenticationFailure => {
                state.phase = Phase::Failed;
                state.message = "Authentication failed".into();
                state.message_is_error = true;
            }
            Fixture::DiscoveryFailure => {
                state.select_accounts(Vec::new());
                state.phase = Phase::Failed;
                state.message = "AccountsService found no unlocked non-system users".into();
                state.message_is_error = true;
            }
            Fixture::SessionFailure => {
                state.session = None;
                state.phase = Phase::Failed;
                state.message = "No valid Wayland sessions are installed".into();
                state.message_is_error = true;
            }
            Fixture::PowerFailure => {
                state.message = "Power action was not authorized".into();
                state.message_is_error = true;
            }
            Fixture::PowerConfirmation => {
                state.power_state = PowerState::Confirming(PowerAction::PowerOff);
            }
        }
        state
    }

    fn select_accounts(&mut self, accounts: Vec<Account>) {
        self.accounts = accounts;
        self.selected = None;
        self.phase = Phase::SelectingUser;
        self.message = "Select a user".into();
    }
}

fn accounts<const N: usize>(values: [(&str, &str); N]) -> Vec<Account> {
    values
        .into_iter()
        .map(|(username, display_name)| {
            Account::override_account(username.into(), Some(display_name.into()))
        })
        .collect()
}

fn preview_session() -> Session {
    Session {
        name: "Preview Wayland".into(),
        command: vec!["preview-session".into()],
        session_id: "preview".into(),
        desktop_names: vec!["Preview".into()],
    }
}

fn preview_now() -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 8, 29, 9, 41, 0)
        .single()
        .expect("preview date must be valid in the local time zone")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_deterministic_and_service_free() {
        for fixture in Fixture::value_variants() {
            let (app, _) = build(*fixture, None, None);
            assert!(app.preview, "fixture {fixture:?}");
            assert!(app.client.is_none(), "fixture {fixture:?}");
            assert_eq!(app.now.format("%-I:%M").to_string(), "9:41");
            assert_eq!(
                app.now.format("%A, %B %-d").to_string(),
                "Saturday, August 29"
            );
            assert_eq!(app.background_elapsed(), 0.0);
        }

        let (selected, _) = build(Fixture::Selected, None, None);
        assert_eq!(selected.username, "preview");
        assert_eq!(selected.display_name, "Preview User");
        assert_eq!(selected.accounts, vec![selected.accounts[0].clone()]);
        assert_eq!(
            selected.selected_session.as_ref().unwrap(),
            &preview_session()
        );
    }

    #[test]
    fn preview_ticks_do_not_change_time_or_animation() {
        let (mut app, _) = build(Fixture::Selected, None, None);
        let now = app.now;

        let _ = app.update(Message::Tick);

        assert_eq!(app.now, now);
        assert_eq!(app.background_elapsed(), 0.0);
    }

    #[test]
    fn account_fixtures_cover_cardinality_and_collisions() {
        let (users, _) = build(Fixture::Users, None, None);
        assert_eq!(users.accounts.len(), 2);
        assert_eq!(users.phase, Phase::SelectingUser);

        let (duplicates, _) = build(Fixture::DuplicateNames, None, None);
        assert_eq!(
            duplicates.accounts[0].display_name,
            duplicates.accounts[1].display_name
        );
        assert_ne!(
            duplicates.accounts[0].username,
            duplicates.accounts[1].username
        );

        let (large, _) = build(Fixture::LargeAccountSet, None, None);
        assert_eq!(large.accounts.len(), 24);

        let (empty, _) = build(Fixture::DiscoveryFailure, None, None);
        assert!(empty.accounts.is_empty());
    }

    #[test]
    fn prompt_and_failure_fixtures_expose_expected_states() {
        let (visible, _) = build(Fixture::VisiblePrompt, None, None);
        assert_eq!(visible.phase, Phase::WaitingForInput);
        assert!(!visible.secret);

        let (authentication, _) = build(Fixture::AuthenticationFailure, None, None);
        assert_eq!(authentication.phase, Phase::Failed);
        assert!(authentication.message_is_error);

        let (cancellation, _) = build(Fixture::CancellationProgress, None, None);
        assert_eq!(cancellation.phase, Phase::CancellingForUserSelection);
        assert!(!cancellation.can_select_account());

        let (cancellation_failure, _) = build(Fixture::CancellationFailure, None, None);
        assert_eq!(
            cancellation_failure.phase,
            Phase::UserSelectionCancellationFailed
        );
        assert!(cancellation_failure.message_is_error);

        let (power, _) = build(Fixture::PowerConfirmation, None, None);
        assert_eq!(
            power.power_state,
            PowerState::Confirming(PowerAction::PowerOff)
        );
    }
}
