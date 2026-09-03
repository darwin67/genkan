use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, responsive, row, scrollable, stack, text, text_input, Space,
};
use iced::{
    padding, Alignment, Color, Element, Fill, Length, Point, Rectangle, Renderer, Size, Theme,
};

use crate::accounts::Account;
use crate::power::Action as PowerAction;
use crate::{background, theme};

use super::account_tile as tile_widget;
use super::auth_flow::Phase;
use super::focus::Target as FocusTarget;
use super::modal as modal_widget;
use super::resettable;
use super::{App, Message, PowerState};

const ACCOUNT_TILE_WIDTH: f32 = 148.0;
const ACCOUNT_GRID_GAP: f32 = 18.0;
const MAX_ACCOUNT_COLUMNS: usize = 4;
const AUTH_ACTION_SIZE: f32 = 34.0;
const AUTH_ACTION_INSET: f32 = 7.0;
const WIDE_MIN_WIDTH: f32 = 1280.0;
const WIDE_MIN_HEIGHT: f32 = 700.0;

#[derive(Debug, Clone, Copy)]
struct SubmitArrow {
    color: Color,
}

fn submit_visual_status(status: button::Status, has_input: bool) -> button::Status {
    if has_input {
        status
    } else {
        button::Status::Disabled
    }
}

impl<Message> canvas::Program<Message> for SubmitArrow {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let path = Path::new(|path| {
            path.move_to(Point::new(center.x - 5.0, center.y));
            path.line_to(Point::new(center.x + 6.0, center.y));
            path.move_to(Point::new(center.x + 2.0, center.y - 4.0));
            path.line_to(Point::new(center.x + 6.0, center.y));
            path.line_to(Point::new(center.x + 2.0, center.y + 4.0));
        });
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(self.color)
                .with_width(2.25)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round),
        );
        vec![frame.into_geometry()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSelectorState {
    Interactive,
    Disabled,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowControl {
    Identity,
    Session,
    Power,
}

const FLOW_CONTROL_ORDER: [FlowControl; 3] = [
    FlowControl::Identity,
    FlowControl::Session,
    FlowControl::Power,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenLayout {
    Wide,
    Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthenticationControls {
    Prompt,
    Retry,
    Progress(&'static str),
    Unavailable,
}

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        let background = self
            .wallpaper
            .view()
            .unwrap_or_else(|| background::Background::new(self.background_elapsed()).view());
        let content = responsive(move |size| self.content(size));
        stack![background, background::dimming(), content].into()
    }

    fn content(&self, size: Size) -> Element<'_, Message> {
        let main_content = self.main_content(size);
        match self.power_state {
            PowerState::Confirming(action) => {
                let dialog_interactive = self.power_dialog_interactive();
                let confirmation = container(
                    column![
                        text(format!("{} this computer?", action.label()))
                            .size(24)
                            .color(theme::primary_text()),
                        row![
                            button("Cancel")
                                .on_press_maybe(dialog_interactive.then_some(Message::CancelPower))
                                .style(|theme, status| {
                                    theme::dialog_button(
                                        theme,
                                        status,
                                        self.is_focused(FocusTarget::DialogCancel),
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
                                        self.is_focused(FocusTarget::DialogConfirm),
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
                .style(theme::dialog);
                modal(main_content, confirmation)
            }
            PowerState::Executing(action) => {
                let progress = container(
                    text(format!("Requesting {}…", action.label().to_lowercase()))
                        .size(22)
                        .color(theme::primary_text()),
                )
                .padding(30)
                .style(theme::dialog);
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
                        Space::new().width(Fill).height(Fill),
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
            ScreenLayout::Flow => {
                let controls = FLOW_CONTROL_ORDER.map(|control| match control {
                    FlowControl::Identity => {
                        self.identity(None, Some((size.width - 32.0).max(0.0)), true)
                    }
                    FlowControl::Session => self.session_selector(),
                    FlowControl::Power => self.power_controls(),
                });
                scrollable(
                    container(
                        column![self.clock(), self.preview_indicator()]
                            .extend(controls)
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
                .style(theme::scrollbar)
                .into()
            }
        }
    }

    fn clock(&self) -> Element<'_, Message> {
        column![
            text(self.now.format("%-I:%M").to_string())
                .size(80)
                .color(theme::primary_text()),
            text(self.now.format("%A, %B %-d").to_string())
                .size(22)
                .color(theme::strong_secondary_text()),
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
                power_button(
                    PowerAction::Suspend,
                    power_interactive,
                    self.is_focused(FocusTarget::Power(PowerAction::Suspend))
                ),
                power_button(
                    PowerAction::Reboot,
                    power_interactive,
                    self.is_focused(FocusTarget::Power(PowerAction::Reboot))
                ),
                power_button(
                    PowerAction::PowerOff,
                    power_interactive,
                    self.is_focused(FocusTarget::Power(PowerAction::PowerOff))
                ),
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
            return Space::new().height(0).into();
        };
        container(text(message).size(13).wrapping(Wrapping::WordOrGlyph))
            .padding([7, 12])
            .max_width(360)
            .style(theme::preview_badge)
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
                .style(|theme, status| {
                    theme::secondary_button(
                        theme,
                        status,
                        self.is_focused(FocusTarget::RetryAccountSelection),
                    )
                })
                .into()
        } else {
            Space::new().height(0).into()
        };

        container(
            column![
                text("Select a user").size(28).color(theme::primary_text()),
                account_grid(
                    &self.accounts,
                    interactive,
                    self.focused_account(),
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
                    let has_input = !self.input.is_empty();
                    let submit = button(
                        Canvas::new(SubmitArrow {
                            color: if has_input {
                                theme::primary_text()
                            } else {
                                theme::muted_text()
                            },
                        })
                        .width(Length::Fixed(18.0))
                        .height(Length::Fixed(18.0)),
                    )
                    .on_press_maybe(interactive.then_some(Message::Submit))
                    .width(Length::Fixed(AUTH_ACTION_SIZE))
                    .height(Length::Fixed(AUTH_ACTION_SIZE))
                    .style(move |theme, status| {
                        let mut style = theme::secondary_button(
                            theme,
                            submit_visual_status(status, has_input),
                            self.is_focused(FocusTarget::Submit),
                        );
                        style.border.radius = (AUTH_ACTION_SIZE / 2.0).into();
                        style
                    });
                    let input = container(stack![
                        text_input("", &self.input)
                            .id(self.input_id.clone())
                            .on_input_maybe(interactive.then_some(Message::InputChanged))
                            .on_submit_maybe(interactive.then_some(Message::Submit))
                            .secure(self.secret)
                            .padding(
                                padding::all(12)
                                    .left(18)
                                    .right(AUTH_ACTION_SIZE + AUTH_ACTION_INSET * 2.0)
                            )
                            .size(18)
                            .width(Fill)
                            .style(theme::input),
                        container(submit)
                            .width(Fill)
                            .height(Fill)
                            .padding(padding::right(AUTH_ACTION_INSET))
                            .align_x(Alignment::End)
                            .align_y(Alignment::Center),
                    ])
                    .id(iced::widget::Id::new("authentication-input-anchor"))
                    .width(Fill);
                    column![
                        text(&self.prompt)
                            .size(15)
                            .color(theme::primary_text())
                            .width(Fill)
                            .align_x(Alignment::Center)
                            .wrapping(Wrapping::WordOrGlyph),
                        input,
                    ]
                    .spacing(8)
                    .into()
                }
                AuthenticationControls::Retry => button(text("Retry").size(16))
                    .on_press_maybe(interactive.then_some(Message::Retry))
                    .padding([12, 18])
                    .style(|theme, status| {
                        theme::primary_button(
                            theme,
                            status,
                            self.is_focused(FocusTarget::RetryAuthentication),
                        )
                    })
                    .into(),
                AuthenticationControls::Progress(label) => {
                    text(label).size(15).color(theme::secondary_text()).into()
                }
                AuthenticationControls::Unavailable => Space::new().height(0).into(),
            };
        let change_user: Element<'_, Message> = if self.can_change_user() {
            button(text("Change User").size(14))
                .on_press(Message::ChangeUser)
                .padding([8, 14])
                .style(|theme, status| {
                    theme::secondary_button(theme, status, self.is_focused(FocusTarget::ChangeUser))
                })
                .into()
        } else {
            Space::new().height(0).into()
        };
        let identity = column![
            text(&self.display_name)
                .size(28)
                .color(theme::primary_text())
                .width(Fill)
                .align_x(Alignment::Center)
                .wrapping(Wrapping::WordOrGlyph),
            text(username)
                .size(14)
                .color(theme::muted_text())
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
        let color = theme::status_text(self.message_is_error);
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
            let pick_list = iced::widget::pick_list(
                self.sessions.as_slice(),
                self.selected_session.as_ref(),
                Message::SelectSession,
            )
            .on_open(Message::SessionMenuOpened)
            .on_close(Message::SessionMenuClosed)
            .padding([9, 14])
            .style(|theme, status| {
                theme::selector(theme, status, self.is_focused(FocusTarget::Session))
            })
            .menu_style(theme::selector_menu)
            .width(Length::Fixed(210.0));
            resettable::reset(self.session_selector_key, pick_list)
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
            .style(theme::inactive_control)
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
                    .style(|theme, status| {
                        theme::secondary_button(
                            theme,
                            status,
                            self.is_focused(FocusTarget::RetrySession),
                        )
                    })
                    .into()
            } else {
                Space::new().height(0).into()
            };
        column![
            text("Session").size(13).color(theme::secondary_text()),
            selector,
            notice(self.session_message.as_deref(), true),
            retry,
        ]
        .max_width(300)
        .spacing(6)
        .into()
    }
}

pub(super) fn authentication_controls(phase: Phase, has_session: bool) -> AuthenticationControls {
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
        return Space::new().height(0).into();
    };
    text(message)
        .size(13)
        .color(theme::status_text(error))
        .wrapping(Wrapping::WordOrGlyph)
        .into()
}

fn account_grid<'a>(
    accounts: &'a [Account],
    interactive: bool,
    focused_account: Option<usize>,
    height: Option<f32>,
    width: Option<f32>,
    scroll_id: &'a iced::widget::Id,
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
            .style(theme::scrollbar)
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
                .color(theme::muted_text())
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
    container(
        text(initials(name))
            .size(u32::from(text_size))
            .color(theme::primary_text()),
    )
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
        modal_widget::barrier(stack![
            container(Space::new().width(Fill).height(Fill)).style(theme::modal_scrim),
            container(dialog)
                .width(Fill)
                .height(Fill)
                .padding(padding::bottom(120))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
        ])
    ]
    .into()
}

fn power_button(
    action: PowerAction,
    interactive: bool,
    focused: bool,
) -> Element<'static, Message> {
    button(text(action.label()).size(14))
        .on_press_maybe(interactive.then_some(Message::AskPower(action)))
        .padding([9, 15])
        .style(move |theme, status| theme::secondary_button(theme, status, focused))
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
    fn empty_response_uses_disabled_submit_appearance() {
        for status in [
            button::Status::Active,
            button::Status::Hovered,
            button::Status::Pressed,
        ] {
            assert_eq!(
                submit_visual_status(status, false),
                button::Status::Disabled
            );
            assert_eq!(submit_visual_status(status, true), status);
        }
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
        assert_eq!(
            FLOW_CONTROL_ORDER,
            [
                FlowControl::Identity,
                FlowControl::Session,
                FlowControl::Power,
            ]
        );
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
