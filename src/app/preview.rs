use std::time::Instant;

use chrono::{Local, TimeZone};
use clap::ValueEnum;
use iced::widget::{scrollable, text_input};
use iced::Task;

use crate::accounts::Account;
use crate::power::Action as PowerAction;
use crate::sessions::Session;
use crate::wallpaper;

use super::auth_flow::{Attempt, Phase};
use super::focus::Target as FocusTarget;
use super::{App, Message, PowerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Fixture {
    Selected,
    SecretPrompt,
    Users,
    DuplicateNames,
    LargeAccountSet,
    LongAccounts,
    LongAuthentication,
    VisiblePrompt,
    InformationalMessage,
    ConsecutiveMessages,
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
    message: Option<String>,
    message_is_error: bool,
    session_message: Option<String>,
    power_message: Option<String>,
    power_message_is_error: bool,
    preview_message: Option<String>,
    secret: bool,
    phase: Phase,
    session: Option<Session>,
    power_state: PowerState,
}

pub(super) fn build(
    fixture: Fixture,
    username: Option<String>,
    display_name: Option<String>,
    wallpaper_settings: wallpaper::Settings,
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
    let power_state = state.power_state;
    let confirming_power = matches!(power_state, PowerState::Confirming(_));
    let mut app = App {
        username,
        display_name,
        accounts: state.accounts,
        focus_target: None,
        focus_before_modal: None,
        account_scroll_id: scrollable::Id::unique(),
        page_scroll_id: scrollable::Id::unique(),
        input: String::new(),
        input_id,
        prompt: state.prompt,
        message: state.message,
        message_is_error: state.message_is_error,
        session_message: state.session_message,
        power_message: state.power_message,
        power_message_is_error: state.power_message_is_error,
        preview_message: state.preview_message,
        secret: state.secret,
        phase: state.phase,
        client: None,
        sessions,
        selected_session,
        session_menu_open: false,
        session_selector_key: 0,
        wallpaper: wallpaper::State::start(wallpaper_settings),
        started_at: Instant::now(),
        now: preview_now(),
        power_state: PowerState::Idle,
        attempt: Attempt::initial(),
        selection_session_cancelled: false,
        closing: None,
        preview: true,
    };
    let base_focus = app.focus_order().first().copied();
    app.power_state = power_state;
    let task = if confirming_power {
        app.focus_before_modal = base_focus;
        app.set_focus(FocusTarget::DialogCancel)
    } else {
        app.focus_first()
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
        let mut state = Self {
            accounts: vec![selected.clone()],
            selected: Some(selected),
            prompt: "Password".into(),
            message: None,
            message_is_error: false,
            session_message: None,
            power_message: None,
            power_message_is_error: false,
            preview_message: Some(
                "Preview mode: credentials and power actions are simulated".into(),
            ),
            secret: true,
            phase: Phase::WaitingForInput,
            session: Some(session),
            power_state: PowerState::Idle,
        };

        match fixture {
            Fixture::Selected | Fixture::SecretPrompt => {}
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
            Fixture::LongAccounts => {
                let prefix = "account".repeat(34);
                state.select_accounts(vec![
                    Account::override_account(
                        format!("{prefix}01"),
                        Some("Long duplicate account name ".repeat(4)),
                    ),
                    Account::override_account(
                        format!("{prefix}02"),
                        Some("Long duplicate account name ".repeat(4)),
                    ),
                ]);
            }
            Fixture::LongAuthentication => {
                let account = Account::override_account(
                    "selected-account".repeat(16),
                    Some("Long selected account name ".repeat(4)),
                );
                state.accounts = vec![account.clone()];
                state.selected = Some(account);
                state.prompt = "Enter the complete authentication challenge: ".to_owned()
                    + &"challenge ".repeat(52);
                state.message = Some("Authentication details: ".to_owned() + &"detail ".repeat(70));
            }
            Fixture::VisiblePrompt => {
                state.prompt = "Verification code".into();
                state.secret = false;
            }
            Fixture::InformationalMessage => {
                state.message = Some("Touch the security key, then enter your password".into());
            }
            Fixture::ConsecutiveMessages => {
                state.message = Some("Security key accepted; enter your password".into());
            }
            Fixture::CancellationProgress => {
                state.select_accounts(accounts([("alice", "Alice"), ("bob", "Bob")]));
                state.phase = Phase::CancellingForUserSelection;
                state.message = Some("Still changing user…".into());
            }
            Fixture::CancellationFailure => {
                state.select_accounts(accounts([("alice", "Alice"), ("bob", "Bob")]));
                state.phase = Phase::UserSelectionCancellationFailed;
                state.message = Some("Could not cancel the previous login attempt".into());
                state.message_is_error = true;
            }
            Fixture::AuthenticationFailure => {
                state.phase = Phase::Failed;
                state.message = Some("Authentication failed".into());
                state.message_is_error = true;
            }
            Fixture::DiscoveryFailure => {
                state.select_accounts(Vec::new());
                state.phase = Phase::Failed;
                state.message = Some("AccountsService found no unlocked non-system users".into());
                state.message_is_error = true;
            }
            Fixture::SessionFailure => {
                state.session = None;
                state.phase = Phase::Failed;
                state.session_message = Some("No valid Wayland sessions are installed".into());
            }
            Fixture::PowerFailure => {
                state.power_message = Some("Power action was not authorized".into());
                state.power_message_is_error = true;
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
        self.message = Some("Select a user".into());
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

    fn build_fixture(fixture: Fixture) -> (App, Task<Message>) {
        build(
            fixture,
            None,
            None,
            wallpaper::Settings {
                catalog: wallpaper::Catalog::TahoeBeach,
                override_path: None,
                animate: false,
            },
        )
    }

    #[test]
    fn fixtures_are_deterministic_and_service_free() {
        for fixture in Fixture::value_variants() {
            let (app, _) = build_fixture(*fixture);
            assert!(app.preview, "fixture {fixture:?}");
            assert!(app.client.is_none(), "fixture {fixture:?}");
            assert!(app.wallpaper.decoder_is_stopped(), "fixture {fixture:?}");
            assert!(app.wallpaper.has_frame(), "fixture {fixture:?}");
            assert_eq!(app.now.format("%-I:%M").to_string(), "9:41");
            assert_eq!(
                app.now.format("%A, %B %-d").to_string(),
                "Saturday, August 29"
            );
            assert_eq!(app.background_elapsed(), 0.0);
        }

        let (selected, _) = build_fixture(Fixture::Selected);
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
        let (mut app, _) = build_fixture(Fixture::Selected);
        let now = app.now;

        let _ = app.update(Message::Tick);

        assert_eq!(app.now, now);
        assert_eq!(app.background_elapsed(), 0.0);
    }

    #[test]
    fn preview_account_changes_clear_simulated_responses_without_a_client() {
        let (mut app, _) = build_fixture(Fixture::Users);
        app.input = "simulated response".into();
        let account = app.accounts[0].clone();

        let _ = app.update(Message::SelectAccount(account));

        assert!(app.input.is_empty());
        assert!(app.client.is_none());
        assert_eq!(app.phase, Phase::WaitingForInput);
        assert!(app.preview_message.is_some());
    }

    #[test]
    fn account_fixtures_cover_cardinality_and_collisions() {
        let (users, _) = build_fixture(Fixture::Users);
        assert_eq!(users.accounts.len(), 2);
        assert_eq!(users.phase, Phase::SelectingUser);

        let (duplicates, _) = build_fixture(Fixture::DuplicateNames);
        assert_eq!(
            duplicates.accounts[0].display_name,
            duplicates.accounts[1].display_name
        );
        assert_ne!(
            duplicates.accounts[0].username,
            duplicates.accounts[1].username
        );

        let (large, _) = build_fixture(Fixture::LargeAccountSet);
        assert_eq!(large.accounts.len(), 24);

        let (long, _) = build_fixture(Fixture::LongAccounts);
        assert_eq!(long.accounts.len(), 2);
        assert_eq!(long.accounts[0].display_name, long.accounts[1].display_name);
        assert_ne!(long.accounts[0].username, long.accounts[1].username);
        assert!(long.accounts[0].display_name.chars().count() >= 70);
        assert!(long.accounts[0].username.chars().count() >= 240);
        assert!(long.requires_flow_layout());

        let (empty, _) = build_fixture(Fixture::DiscoveryFailure);
        assert!(empty.accounts.is_empty());
    }

    #[test]
    fn prompt_and_failure_fixtures_expose_expected_states() {
        let (visible, _) = build_fixture(Fixture::VisiblePrompt);
        assert_eq!(visible.phase, Phase::WaitingForInput);
        assert!(!visible.secret);

        let (long, _) = build_fixture(Fixture::LongAuthentication);
        assert!(long.prompt.chars().count() >= 500);
        assert!(long.message.as_deref().unwrap().chars().count() >= 500);
        assert!(long.username.chars().count() >= 240);
        assert!(long.requires_flow_layout());

        let (secret, _) = build_fixture(Fixture::SecretPrompt);
        assert!(secret.secret);

        let (consecutive, _) = build_fixture(Fixture::ConsecutiveMessages);
        assert_eq!(
            consecutive.message.as_deref(),
            Some("Security key accepted; enter your password")
        );

        let (authentication, _) = build_fixture(Fixture::AuthenticationFailure);
        assert_eq!(authentication.phase, Phase::Failed);
        assert!(authentication.message_is_error);
        assert_eq!(
            authentication.focus_target,
            Some(FocusTarget::RetryAuthentication)
        );

        let (discovery, _) = build_fixture(Fixture::DiscoveryFailure);
        assert_eq!(
            discovery.focus_target,
            Some(FocusTarget::RetryAccountSelection)
        );

        let (session, _) = build_fixture(Fixture::SessionFailure);
        assert_eq!(session.focus_target, Some(FocusTarget::RetrySession));

        let (cancellation, _) = build_fixture(Fixture::CancellationProgress);
        assert_eq!(cancellation.phase, Phase::CancellingForUserSelection);
        assert!(!cancellation.can_select_account());
        assert_eq!(cancellation.focus_target, Some(FocusTarget::Session));

        let (cancellation_failure, _) = build_fixture(Fixture::CancellationFailure);
        assert_eq!(
            cancellation_failure.phase,
            Phase::UserSelectionCancellationFailed
        );
        assert!(cancellation_failure.message_is_error);
        assert_eq!(
            cancellation_failure.focus_target,
            Some(FocusTarget::RetryAccountSelection)
        );

        let (power, _) = build_fixture(Fixture::PowerConfirmation);
        assert_eq!(
            power.power_state,
            PowerState::Confirming(PowerAction::PowerOff)
        );
        assert_eq!(power.focus_target, Some(FocusTarget::DialogCancel));
        assert_eq!(
            power.focus_before_modal,
            Some(FocusTarget::AuthenticationInput)
        );
    }
}
