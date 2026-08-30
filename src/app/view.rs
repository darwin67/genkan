use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, responsive, row, scrollable, stack, text, text_input, Space,
};
use iced::{Alignment, Color, Element, Fill, Length, Size};

use crate::accounts::Account;
use crate::power::Action as PowerAction;
use crate::{background, theme};

use super::account_tile as tile_widget;
use super::auth_flow::Phase;
use super::{App, Message, PowerDialogFocus, PowerState};

const ACCOUNT_TILE_WIDTH: f32 = 148.0;
const ACCOUNT_GRID_GAP: f32 = 18.0;
const MAX_ACCOUNT_COLUMNS: usize = 4;
const AUTH_ACTION_WIDTH: f32 = 82.0;
const WIDE_MIN_WIDTH: f32 = 1280.0;
const WIDE_MIN_HEIGHT: f32 = 700.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSelectorState {
    Interactive,
    Disabled,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenLayout {
    Wide,
    Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthenticationControls {
    Prompt,
    Retry,
    Progress(&'static str),
    Unavailable,
}

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        let background = background::Background::new(self.background_elapsed()).view();
        let content = responsive(move |size| self.content(size));
        stack![background, content].into()
    }

    fn content(&self, size: Size) -> Element<'_, Message> {
        let main_content = self.main_content(size);
        match self.power_state {
            PowerState::Confirming(action) => {
                let dialog_interactive = self.power_dialog_interactive();
                let confirmation = container(
                    column![
                        text(format!("{} this computer?", action.label())).size(24),
                        row![
                            button("Cancel")
                                .on_press_maybe(dialog_interactive.then_some(Message::CancelPower))
                                .style(|theme, status| {
                                    theme::dialog_button(
                                        theme,
                                        status,
                                        self.power_dialog_focus == PowerDialogFocus::Cancel,
                                        false,
                                    )
                                }),
                            button(action.label())
                                .on_press_maybe(
                                    dialog_interactive.then_some(Message::ConfirmPower(action))
                                )
                                .style(|theme, status| {
                                    theme::dialog_button(
                                        theme,
                                        status,
                                        self.power_dialog_focus == PowerDialogFocus::Confirm,
                                        true,
                                    )
                                }),
                        ]
                        .spacing(12),
                    ]
                    .align_x(Alignment::Center)
                    .spacing(22),
                )
                .padding(30)
                .style(theme::panel);
                modal(main_content, confirmation)
            }
            PowerState::Executing(action) => {
                let progress = container(
                    text(format!("Requesting {}…", action.label().to_lowercase())).size(22),
                )
                .padding(30)
                .style(theme::panel);
                modal(main_content, progress)
            }
            PowerState::Idle => main_content,
        }
    }

    fn main_content(&self, size: Size) -> Element<'_, Message> {
        let layout = if self.requires_flow_layout() {
            ScreenLayout::Flow
        } else {
            screen_layout(size)
        };
        match layout {
            ScreenLayout::Wide => {
                let center = container(
                    column![
                        self.clock(),
                        Space::new(Fill, Fill),
                        self.identity(Some(292.0), None, false)
                    ]
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Center),
                )
                .width(Fill)
                .height(Fill)
                .padding([42, 24]);
                let utilities = container(self.power_controls())
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::End)
                    .align_y(Alignment::Start)
                    .padding([28, 30]);
                let session = container(self.session_selector())
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::End)
                    .padding([28, 30]);
                let preview = container(self.preview_indicator())
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::Start)
                    .padding([28, 30]);
                stack![center, utilities, session, preview].into()
            }
            ScreenLayout::Flow => scrollable(
                container(
                    column![
                        self.clock(),
                        self.preview_indicator(),
                        self.power_controls(),
                        self.identity(None, Some((size.width - 32.0).max(0.0)), true),
                        self.session_selector(),
                    ]
                    .width(Fill)
                    .align_x(Alignment::Center)
                    .spacing(28),
                )
                .width(Fill)
                .padding([24, 16]),
            )
            .id(self.page_scroll_id.clone())
            .width(Fill)
            .height(Fill)
            .into(),
        }
    }

    fn clock(&self) -> Element<'_, Message> {
        column![
            text(self.now.format("%-I:%M").to_string())
                .size(80)
                .color(Color::WHITE),
            text(self.now.format("%A, %B %-d").to_string())
                .size(22)
                .color(Color::from_rgba8(255, 255, 255, 0.85)),
        ]
        .align_x(Alignment::Center)
        .spacing(0)
        .into()
    }

    fn identity(
        &self,
        grid_height: Option<f32>,
        grid_width: Option<f32>,
        flow: bool,
    ) -> Element<'_, Message> {
        let selector_state = account_selector_state(
            self.phase,
            self.can_select_account(),
            !self.username.is_empty(),
        );
        match selector_state {
            AccountSelectorState::Interactive => {
                self.account_selection(true, grid_height, grid_width, flow)
            }
            AccountSelectorState::Disabled => {
                self.account_selection(false, grid_height, grid_width, flow)
            }
            AccountSelectorState::Hidden => self.authentication(flow),
        }
    }

    fn power_controls(&self) -> Element<'_, Message> {
        let power_interactive = self.can_request_power();
        column![
            row![
                power_button(PowerAction::Suspend, power_interactive),
                power_button(PowerAction::Reboot, power_interactive),
                power_button(PowerAction::PowerOff, power_interactive),
            ]
            .spacing(10),
            notice(self.power_message.as_deref(), self.power_message_is_error),
        ]
        .align_x(Alignment::End)
        .max_width(360)
        .spacing(6)
        .into()
    }

    fn preview_indicator(&self) -> Element<'_, Message> {
        let Some(message) = self.preview_message.as_deref() else {
            return Space::new(Length::Shrink, Length::Fixed(0.0)).into();
        };
        container(text(message).size(13).wrapping(Wrapping::WordOrGlyph))
            .padding([7, 12])
            .max_width(360)
            .style(theme::selection)
            .into()
    }

    fn account_selection(
        &self,
        interactive: bool,
        grid_height: Option<f32>,
        grid_width: Option<f32>,
        flow: bool,
    ) -> Element<'_, Message> {
        let status = self.status_for(
            self.message
                .as_deref()
                .filter(|message| *message != "Select a user"),
            flow,
        );
        let retry: Element<'_, Message> = if self.phase == Phase::UserSelectionCancellationFailed
            || (self.phase == Phase::Failed
                && self.username.is_empty()
                && self.selected_session.is_some())
        {
            let (label, message) = if self.phase == Phase::UserSelectionCancellationFailed {
                (
                    "Retry changing user",
                    Message::RetryUserSelectionCancellation,
                )
            } else {
                ("Retry account discovery", Message::Retry)
            };
            button(text(label).size(15))
                .on_press_maybe(
                    (self.closing.is_none() && self.power_state == PowerState::Idle)
                        .then_some(message),
                )
                .padding([10, 18])
                .style(theme::translucent_button)
                .into()
        } else {
            Space::new(Length::Shrink, Length::Fixed(0.0)).into()
        };

        container(
            column![
                text("Select a user").size(28).color(Color::WHITE),
                account_grid(
                    &self.accounts,
                    interactive,
                    self.focused_account,
                    grid_height,
                    grid_width,
                    &self.account_scroll_id,
                ),
                status,
                retry,
            ]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(14),
        )
        .width(Fill)
        .max_width(780)
        .into()
    }

    fn authentication(&self, flow: bool) -> Element<'_, Message> {
        let interactive = self.closing.is_none() && self.power_state == PowerState::Idle;
        let selected_account = self
            .accounts
            .iter()
            .find(|account| account.username == self.username);
        let username = selected_account
            .map(|account| format!("@{}", account.username))
            .unwrap_or_else(|| format!("@{}", self.username));
        let controls: Element<'_, Message> =
            match authentication_controls(self.phase, self.selected_session.is_some()) {
                AuthenticationControls::Prompt => {
                    let input = container(
                        text_input("", &self.input)
                            .id(self.input_id.clone())
                            .on_input_maybe(interactive.then_some(Message::InputChanged))
                            .on_submit_maybe(interactive.then_some(Message::Submit))
                            .secure(self.secret)
                            .padding([12, 18])
                            .size(18)
                            .width(Fill)
                            .style(theme::input),
                    )
                    .id(iced::widget::container::Id::new(
                        "authentication-input-anchor",
                    ))
                    .width(Fill);
                    column![
                        text(&self.prompt)
                            .size(15)
                            .width(Fill)
                            .align_x(Alignment::Center)
                            .wrapping(Wrapping::WordOrGlyph),
                        row![
                            Space::new(Length::Fixed(AUTH_ACTION_WIDTH), Length::Shrink),
                            input,
                            button(text("Log In").size(16))
                                .on_press_maybe(interactive.then_some(Message::Submit))
                                .padding([12, 18])
                                .width(Length::Fixed(AUTH_ACTION_WIDTH))
                                .style(theme::primary_button),
                        ]
                        .spacing(8)
                        .width(Fill),
                    ]
                    .spacing(8)
                    .into()
                }
                AuthenticationControls::Retry => button(text("Retry").size(16))
                    .on_press_maybe(interactive.then_some(Message::Retry))
                    .padding([12, 18])
                    .style(theme::primary_button)
                    .into(),
                AuthenticationControls::Progress(label) => text(label)
                    .size(15)
                    .color(Color::from_rgba8(255, 255, 255, 0.78))
                    .into(),
                AuthenticationControls::Unavailable => {
                    Space::new(Length::Shrink, Length::Fixed(0.0)).into()
                }
            };
        let change_user: Element<'_, Message> = if self.can_change_user() {
            button(text("Change User").size(14))
                .on_press(Message::ChangeUser)
                .padding([8, 14])
                .style(theme::translucent_button)
                .into()
        } else {
            Space::new(Length::Shrink, Length::Fixed(0.0)).into()
        };
        let identity = column![
            text(&self.display_name)
                .size(28)
                .color(Color::WHITE)
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
            text(username)
                .size(14)
                .color(Color::from_rgba8(255, 255, 255, 0.68))
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .width(Fill)
        .align_x(Alignment::Center)
        .spacing(2);
        let identity: Element<'_, Message> = identity.into();

        container(
            column![
                avatar(&self.display_name, 100.0, 38),
                identity,
                column![controls, self.status_for(self.message.as_deref(), flow)]
                    .width(Fill)
                    .align_x(Alignment::Center)
                    .spacing(8),
                change_user,
            ]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(14),
        )
        .width(Fill)
        .max_width(540)
        .into()
    }

    pub(super) fn requires_flow_layout(&self) -> bool {
        dynamic_text_requires_flow(&self.prompt)
            || self
                .message
                .as_deref()
                .is_some_and(dynamic_text_requires_flow)
            || dynamic_text_requires_flow(&self.display_name)
            || dynamic_text_requires_flow(&self.username)
            || self.accounts.iter().any(|account| {
                dynamic_text_requires_flow(&account.display_name)
                    || dynamic_text_requires_flow(&account.username)
            })
            || self
                .session_message
                .as_deref()
                .is_some_and(dynamic_text_requires_flow)
            || self
                .power_message
                .as_deref()
                .is_some_and(dynamic_text_requires_flow)
            || self
                .preview_message
                .as_deref()
                .is_some_and(dynamic_text_requires_flow)
    }

    fn status_for<'a>(&'a self, message: Option<&'a str>, _flow: bool) -> Element<'a, Message> {
        let status = message.unwrap_or(" ");
        let color = if self.message_is_error {
            Color::from_rgb8(255, 171, 171)
        } else {
            Color::from_rgba8(255, 255, 255, 0.78)
        };
        let status = text(status)
            .size(14)
            .color(color)
            .width(Fill)
            .align_x(Alignment::Center)
            .wrapping(Wrapping::WordOrGlyph);
        status.into()
    }

    fn session_selector(&self) -> Element<'_, Message> {
        let selector: Element<'_, Message> = if self.can_select_session() {
            iced::widget::pick_list(
                self.sessions.as_slice(),
                self.selected_session.as_ref(),
                Message::SelectSession,
            )
            .padding([9, 14])
            .style(theme::selector)
            .menu_style(theme::selector_menu)
            .width(Length::Fixed(210.0))
            .into()
        } else {
            container(
                text(
                    self.selected_session
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "No session available".into()),
                )
                .size(15),
            )
            .width(Length::Fixed(210.0))
            .padding([9, 14])
            .style(theme::selection)
            .into()
        };
        let retry: Element<'_, Message> =
            if self.phase == Phase::Failed && self.selected_session.is_none() {
                button(text("Retry session discovery").size(13))
                    .on_press_maybe(
                        (self.closing.is_none() && self.power_state == PowerState::Idle)
                            .then_some(Message::RetrySession),
                    )
                    .padding([7, 12])
                    .style(theme::translucent_button)
                    .into()
            } else {
                Space::new(Length::Shrink, Length::Fixed(0.0)).into()
            };
        column![
            text("Session")
                .size(13)
                .color(Color::from_rgba8(255, 255, 255, 0.72)),
            selector,
            notice(self.session_message.as_deref(), true),
            retry,
        ]
        .max_width(300)
        .spacing(6)
        .into()
    }
}

fn authentication_controls(phase: Phase, has_session: bool) -> AuthenticationControls {
    match phase {
        Phase::WaitingForInput => AuthenticationControls::Prompt,
        Phase::Failed if has_session => AuthenticationControls::Retry,
        Phase::Failed => AuthenticationControls::Unavailable,
        Phase::CreatingSession => AuthenticationControls::Progress("Preparing authentication…"),
        Phase::Authenticating => AuthenticationControls::Progress("Continuing authentication…"),
        Phase::StartingSession => AuthenticationControls::Progress("Starting session…"),
        Phase::DiscoveringUsers
        | Phase::CancellingForUserSelection
        | Phase::SelectingUser
        | Phase::UserSelectionCancellationFailed => AuthenticationControls::Unavailable,
    }
}

fn notice<'a>(message: Option<&'a str>, error: bool) -> Element<'a, Message> {
    let Some(message) = message else {
        return Space::new(Length::Shrink, Length::Fixed(0.0)).into();
    };
    text(message)
        .size(13)
        .color(if error {
            Color::from_rgb8(255, 171, 171)
        } else {
            Color::from_rgba8(255, 255, 255, 0.78)
        })
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

fn account_grid<'a>(
    accounts: &'a [Account],
    interactive: bool,
    focused_account: Option<usize>,
    height: Option<f32>,
    width: Option<f32>,
    scroll_id: &'a scrollable::Id,
) -> Element<'a, Message> {
    if let Some(height) = height {
        container(responsive(move |size| {
            scrollable(account_rows(
                accounts,
                interactive,
                focused_account,
                account_grid_columns(size.width),
            ))
            .id(scroll_id.clone())
            .width(Fill)
            .height(Fill)
            .into()
        }))
        .width(Fill)
        .height(Length::Fixed(height))
        .into()
    } else {
        account_rows(
            accounts,
            interactive,
            focused_account,
            account_grid_columns(width.unwrap_or(ACCOUNT_TILE_WIDTH)),
        )
        .into()
    }
}

fn account_rows<'a>(
    accounts: &'a [Account],
    interactive: bool,
    focused_account: Option<usize>,
    columns: usize,
) -> iced::widget::Column<'a, Message> {
    let rows = accounts
        .chunks(columns)
        .enumerate()
        .map(|(row_index, accounts)| {
            row(accounts.iter().enumerate().map(|(column_index, account)| {
                let index = row_index * columns + column_index;
                account_tile(account, interactive, focused_account == Some(index))
            }))
            .spacing(ACCOUNT_GRID_GAP)
            .align_y(Alignment::Start)
        })
        .map(Element::from)
        .collect::<Vec<_>>();
    column(rows)
        .width(Fill)
        .align_x(Alignment::Center)
        .spacing(ACCOUNT_GRID_GAP)
}

fn account_tile<'a>(
    account: &'a Account,
    interactive: bool,
    focused: bool,
) -> Element<'a, Message> {
    tile_widget::tile(
        column![
            avatar(&account.display_name, 76.0, 28),
            text(&account.display_name)
                .size(16)
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
            text(format!("@{}", account.username))
                .size(13)
                .color(Color::from_rgba8(255, 255, 255, 0.68))
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .width(Fill)
        .align_x(Alignment::Center)
        .spacing(7),
        interactive.then(|| Message::SelectAccount(account.clone())),
        focused,
        ACCOUNT_TILE_WIDTH,
        tile_widget::id(&account.username),
    )
}

fn dynamic_text_requires_flow(value: &str) -> bool {
    value.contains(['\n', '\r']) || !value.is_ascii() || value.chars().count() > 80
}

fn avatar<'a>(name: &str, diameter: f32, text_size: u16) -> Element<'a, Message> {
    container(text(initials(name)).size(text_size).color(Color::WHITE))
        .width(Length::Fixed(diameter))
        .height(Length::Fixed(diameter))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_| theme::avatar(diameter / 2.0))
        .into()
}

fn account_grid_columns(width: f32) -> usize {
    (((width + ACCOUNT_GRID_GAP) / (ACCOUNT_TILE_WIDTH + ACCOUNT_GRID_GAP)) as usize)
        .clamp(1, MAX_ACCOUNT_COLUMNS)
}

fn screen_layout(size: Size) -> ScreenLayout {
    if size.width >= WIDE_MIN_WIDTH && size.height >= WIDE_MIN_HEIGHT {
        ScreenLayout::Wide
    } else {
        ScreenLayout::Flow
    }
}

fn account_selector_state(
    phase: Phase,
    interactive: bool,
    has_identity: bool,
) -> AccountSelectorState {
    match phase {
        Phase::SelectingUser if interactive => AccountSelectorState::Interactive,
        Phase::SelectingUser
        | Phase::CancellingForUserSelection
        | Phase::UserSelectionCancellationFailed => AccountSelectorState::Disabled,
        Phase::Failed if !has_identity => AccountSelectorState::Disabled,
        _ => AccountSelectorState::Hidden,
    }
}

fn modal<'a>(
    main_content: impl Into<Element<'a, Message>>,
    dialog: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    stack![
        main_content.into(),
        container(dialog)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
    ]
    .into()
}

fn power_button(action: PowerAction, interactive: bool) -> Element<'static, Message> {
    button(text(action.label()).size(14))
        .on_press_maybe(interactive.then_some(Message::AskPower(action)))
        .padding([9, 15])
        .style(theme::translucent_button)
        .into()
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_selector_is_disabled_until_cancellation_succeeds() {
        assert_eq!(
            account_selector_state(Phase::CancellingForUserSelection, false, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::UserSelectionCancellationFailed, false, false),
            AccountSelectorState::Disabled
        );
        assert_eq!(
            account_selector_state(Phase::SelectingUser, true, false),
            AccountSelectorState::Interactive
        );
        assert_eq!(
            account_selector_state(Phase::WaitingForInput, false, true),
            AccountSelectorState::Hidden
        );
        assert_eq!(
            account_selector_state(Phase::Failed, false, false),
            AccountSelectorState::Disabled
        );
    }

    #[test]
    fn authentication_only_collects_input_for_protocol_prompts() {
        assert_eq!(
            authentication_controls(Phase::WaitingForInput, true),
            AuthenticationControls::Prompt
        );
        assert_eq!(
            authentication_controls(Phase::Authenticating, true),
            AuthenticationControls::Progress("Continuing authentication…")
        );
        assert_eq!(
            authentication_controls(Phase::Failed, false),
            AuthenticationControls::Unavailable
        );
        assert_eq!(
            authentication_controls(Phase::Failed, true),
            AuthenticationControls::Retry
        );
    }

    #[test]
    fn account_grid_wraps_at_narrow_and_wide_sizes() {
        assert_eq!(account_grid_columns(140.0), 1);
        assert_eq!(account_grid_columns(320.0), 2);
        assert_eq!(account_grid_columns(520.0), 3);
        assert_eq!(account_grid_columns(900.0), MAX_ACCOUNT_COLUMNS);
    }

    #[test]
    fn compact_outputs_use_non_overlapping_flow_layout() {
        assert_eq!(screen_layout(Size::new(1280.0, 800.0)), ScreenLayout::Wide);
        assert_eq!(screen_layout(Size::new(1024.0, 768.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(700.0, 700.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(640.0, 600.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(480.0, 600.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(1280.0, 600.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(1279.0, 700.0)), ScreenLayout::Flow);
        assert_eq!(screen_layout(Size::new(1280.0, 699.0)), ScreenLayout::Flow);
    }

    #[test]
    fn wide_glyphs_and_explicit_lines_require_flow_layout() {
        assert!(dynamic_text_requires_flow("密碼を入力してください"));
        assert!(dynamic_text_requires_flow("first line\nsecond line"));
        assert!(!dynamic_text_requires_flow("Password"));
    }

    #[test]
    fn creates_initials() {
        assert_eq!(initials("Darwin Wu"), "DW");
        assert_eq!(initials("Darwin"), "D");
    }
}
