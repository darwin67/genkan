//! The login screen, composed from the design system's components: the
//! StatusBar, Clock and identity column (or AccountRow) over an undimmed
//! wallpaper, with Menus and the power Alert floating above them.

use std::f32::consts::TAU;
use std::time::Duration;

use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, float, mouse_area, opaque, pin, responsive, row, scrollable, stack,
    text_input, Id, Space, Stack,
};
use iced::{padding, Alignment, Element, Fill, Length, Padding, Size, Vector};

use crate::accounts::Account;
use crate::background;
use crate::design::glyph::{self, Kind as Glyph};
use crate::design::iced::{color, font, label, line_height, scaled_label};
use crate::design::style;
use crate::design::text::{self as text_policy, initials, prompt_placement, PromptPlacement};
use crate::design::tokens::{
    color as palette, component, motion, radius, size, spacing, text_style, Rgba, TextStyle,
};
use crate::power::Action as PowerAction;

use super::account_tile as tile_widget;
use super::auth_flow::Phase;
use super::focus::Target as FocusTarget;
use super::modal as modal_widget;
use super::{App, Menu, Message, PowerState, POWER_MENU};

const POWER_CONSEQUENCE: &str = "Anyone still signed in will lose unsaved work.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSelectorState {
    Interactive,
    Disabled,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthenticationControls {
    Prompt,
    Retry,
    Progress(&'static str),
    Unavailable,
}

/// Where the clock sits once it has made room for the identity column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClockLayout {
    pub(super) scale: f32,
    pub(super) top: f32,
    pub(super) bottom: f32,
}

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        // The wallpaper is never dimmed; only an alert's scrim darkens it.
        let background = self
            .wallpaper
            .view()
            .unwrap_or_else(|| background::Background::new(self.background_elapsed()).view());
        let foreground = responsive(move |size| self.positioned_foreground(size));
        stack![background, foreground].into()
    }

    fn positioned_foreground(&self, size: Size) -> Element<'_, Message> {
        let region = authentication_region_or_full(self.authentication_region, size);
        let region = region.scale_to(size.width, size.height);
        column![
            Space::new().height(region.y),
            row![
                Space::new().width(region.x),
                container(self.content(Size::new(region.width, region.height)))
                    .width(region.width)
                    .height(region.height)
                    .clip(true)
            ]
        ]
        .width(Fill)
        .height(Fill)
        .into()
    }

    fn content(&self, size: Size) -> Element<'_, Message> {
        let time = self.now.format("%-I:%M").to_string();
        let clock = clock_layout(size, text_policy::measure(&time, text_style::CLOCK));
        let mut layers = vec![
            self.clock(time, clock),
            self.identity(size, clock),
            self.status_bar(size),
            self.preview_badge(size),
        ];
        layers.extend(self.menu(size));
        layers.extend(self.alert());
        Stack::with_children(layers).width(Fill).height(Fill).into()
    }

    fn clock(&self, time: String, layout: ClockLayout) -> Element<'_, Message> {
        column![
            Space::new().height(layout.top),
            scaled_label(
                self.now.format("%a %b %-d").to_string(),
                text_style::DATE,
                palette::TEXT_DATE,
                layout.scale,
            ),
            scaled_label(time, text_style::CLOCK, palette::CLOCK_GLASS, layout.scale),
        ]
        .width(Fill)
        .align_x(Alignment::Center)
        .into()
    }

    /// The identity column, or the AccountRow that replaces it, anchored so
    /// the pill's bottom edge sits `identity-bottom` above the region's.
    fn identity(&self, size: Size, clock: ClockLayout) -> Element<'_, Message> {
        let state = account_selector_state(
            self.phase,
            self.can_select_account(),
            !self.username.is_empty(),
        );
        let mut identity = column![Space::new().height(clock.bottom + spacing::IDENTITY_GAP)]
            .width(Fill)
            .height(Fill);
        identity = match state {
            AccountSelectorState::Interactive => identity.push(self.account_row(size.width, true)),
            AccountSelectorState::Disabled => identity.push(self.account_row(size.width, false)),
            AccountSelectorState::Hidden => identity
                .push(self.identity_column())
                .push(Space::new().height(spacing::IDENTITY_GAP))
                .push(self.authentication_slot()),
        };
        identity.push(self.below_anchor(state)).into()
    }

    fn identity_column(&self) -> Element<'_, Message> {
        let mut name = column![label(
            text_policy::fit(&self.display_name, text_style::NAME, size::MESSAGE_WIDTH),
            text_style::NAME,
            palette::TEXT_PRIMARY,
        )]
        .align_x(Alignment::Center);
        if let Some(username) = self.selected_username_if_ambiguous() {
            name = name.push(label(
                text_policy::fit(username, text_style::MESSAGE, size::MESSAGE_WIDTH),
                text_style::MESSAGE,
                palette::TEXT_SECONDARY,
            ));
        }
        let mut items = column![avatar(&self.display_name), name]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(spacing::IDENTITY_GAP);
        if self.phase == Phase::WaitingForInput {
            if let PromptPlacement::Above(prompt) = self.prompt_placement() {
                items = items.push(message(prompt));
            }
        }
        // Long PAM text scrolls here; the pill below always stays visible.
        container(
            scrollable(items)
                .id(self.page_scroll_id.clone())
                .anchor_bottom()
                .width(Fill)
                .height(Length::Shrink)
                .style(style::scrollbar),
        )
        .width(Fill)
        .height(Fill)
        .align_y(Alignment::End)
        .into()
    }

    fn prompt_placement(&self) -> PromptPlacement<'_> {
        prompt_placement(
            self.conversation.prompt(),
            self.conversation.is_secret(),
            |prompt| {
                text_policy::measure(prompt, text_style::FIELD)
                    <= size::FIELD_WIDTH
                        - component::FIELD_PADDING_LEADING
                        - component::FIELD_PADDING_TRAILING
            },
        )
    }

    fn selected_username_if_ambiguous(&self) -> Option<&str> {
        let ambiguous = ambiguous_names(&self.accounts);
        self.accounts
            .iter()
            .zip(ambiguous)
            .find(|(account, _)| account.username == self.username)
            .and_then(|(account, ambiguous)| ambiguous.then_some(account.username.as_str()))
    }

    /// The pill, or what replaces it while no prompt is waiting.
    fn authentication_slot(&self) -> Element<'_, Message> {
        let interactive = self.closing.is_none() && self.power_state == PowerState::Idle;
        match authentication_controls(self.phase, self.selected_session.is_some()) {
            AuthenticationControls::Prompt => {
                let placeholder = match self.prompt_placement() {
                    PromptPlacement::Placeholder(prompt) => prompt,
                    PromptPlacement::Above(_) => "",
                };
                self.pill_row(self.pill(placeholder, interactive))
            }
            // The pill stays in place, empty and disabled, so the rejection
            // shake and the next prompt do not move the column.
            AuthenticationControls::Progress(_) => self.pill_row(self.pill("", false)),
            AuthenticationControls::Retry => container(small_pill(
                "Retry",
                interactive.then_some(Message::Retry),
                self.is_focused(FocusTarget::RetryAuthentication),
            ))
            .width(Fill)
            .align_x(Alignment::Center)
            .into(),
            AuthenticationControls::Unavailable => Space::new().height(size::FIELD_HEIGHT).into(),
        }
    }

    /// The pill centered, with the Change User back button to its left.
    fn pill_row<'a>(&'a self, pill: Element<'a, Message>) -> Element<'a, Message> {
        let offset = self.shake_offset();
        let pill: Element<'a, Message> = float(pill)
            .translate(move |_, _| Vector::new(offset, 0.0))
            .into();
        let content: Element<'a, Message> = if self.can_change_user() {
            let focused = self.is_focused(FocusTarget::ChangeUser);
            let back = button(
                container(glyph::glyph(Glyph::ArrowLeft, color(palette::TEXT_PRIMARY)))
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            .on_press(Message::ChangeUser)
            .width(size::FIELD_HEIGHT)
            .height(size::FIELD_HEIGHT)
            .padding(0)
            .style(move |_, _| style::circle(size::FIELD_HEIGHT, focused));
            // A matching space on the right keeps the pill itself centered.
            row![back, pill, Space::new().width(size::FIELD_HEIGHT)]
                .spacing(spacing::IDENTITY_GAP)
                .align_y(Alignment::Center)
                .into()
        } else {
            pill
        };
        container(content)
            .width(Fill)
            .align_x(Alignment::Center)
            .into()
    }

    fn pill<'a>(&'a self, placeholder: &'a str, interactive: bool) -> Element<'a, Message> {
        let input = text_input(placeholder, self.conversation.input())
            .id(self.input_id.clone())
            .on_input_maybe(interactive.then_some(Message::InputChanged))
            .on_submit_maybe(interactive.then_some(Message::Submit))
            .secure(self.conversation.is_secret())
            .font(font(text_style::FIELD))
            .size(text_style::FIELD.size)
            .line_height(line_height(text_style::FIELD))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: component::FIELD_PADDING_LEADING,
                right: component::FIELD_PADDING_TRAILING,
            })
            .width(Fill)
            .style(style::field);
        let mut layers: Vec<Element<'a, Message>> = vec![input.into()];
        if !self.conversation.input().is_empty() {
            let focused = self.is_focused(FocusTarget::Submit);
            let submit = button(
                container(glyph::glyph(
                    Glyph::ArrowRight,
                    color(palette::TEXT_PRIMARY),
                ))
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
            )
            .on_press_maybe(interactive.then_some(Message::Submit))
            .width(size::SUBMIT)
            .height(size::SUBMIT)
            .padding(0)
            .style(move |_, _| style::circle(size::SUBMIT, focused));
            layers.push(
                container(submit)
                    .width(Fill)
                    .height(Fill)
                    .padding(padding::right(component::SUBMIT_INSET))
                    .align_x(Alignment::End)
                    .align_y(Alignment::Center)
                    .into(),
            );
        }
        container(Stack::with_children(layers))
            .id(Id::new("authentication-input-anchor"))
            .width(size::FIELD_WIDTH)
            .height(size::FIELD_HEIGHT)
            .into()
    }

    /// The `identity-bottom` band under the pill: PAM notices, progress, the
    /// power status, and recovery actions for account selection.
    fn below_anchor(&self, state: AccountSelectorState) -> Element<'_, Message> {
        let mut lines = column![]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(spacing::IDENTITY_GAP);
        match state {
            AccountSelectorState::Hidden => {
                if let AuthenticationControls::Progress(progress) =
                    authentication_controls(self.phase, self.selected_session.is_some())
                {
                    lines = lines.push(message(progress));
                }
                if let Some(notice) = self.conversation.notice() {
                    lines = lines.push(message(notice));
                }
            }
            AccountSelectorState::Interactive | AccountSelectorState::Disabled => {
                if let Some(notice) = self
                    .conversation
                    .notice()
                    .filter(|notice| *notice != "Select a user")
                {
                    lines = lines.push(message(notice));
                }
                if let Some((label, retry)) = self.account_retry() {
                    lines = lines.push(small_pill(
                        label,
                        (self.closing.is_none() && self.power_state == PowerState::Idle)
                            .then_some(retry),
                        self.is_focused(FocusTarget::RetryAccountSelection),
                    ));
                }
            }
        }
        if let Some(status) = self.power_status() {
            lines = lines.push(message(status));
        }
        column![
            Space::new().height(spacing::IDENTITY_GAP),
            scrollable(lines)
                .width(Fill)
                .height(Fill)
                .style(style::scrollbar),
        ]
        .width(Fill)
        .height(spacing::IDENTITY_BOTTOM)
        .into()
    }

    fn account_retry(&self) -> Option<(&'static str, Message)> {
        if self.phase == Phase::UserSelectionCancellationFailed {
            Some((
                "Retry Changing User",
                Message::RetryUserSelectionCancellation,
            ))
        } else if self.phase == Phase::Failed
            && self.username.is_empty()
            && self.selected_session.is_some()
        {
            Some(("Retry Account Discovery", Message::Retry))
        } else {
            None
        }
    }

    /// Power progress or failure shown under the pill. Restart and Shut Down
    /// report progress inside their alert instead.
    fn power_status(&self) -> Option<&str> {
        match self.power_state {
            PowerState::Executing(PowerAction::Suspend) => Some("Requesting sleep…"),
            PowerState::Idle => self.power_message.as_deref(),
            _ => None,
        }
    }

    fn account_row(&self, width: f32, interactive: bool) -> Element<'_, Message> {
        let names = self
            .accounts
            .iter()
            .map(|account| account.display_name.as_str())
            .collect::<Vec<_>>();
        let usernames = self
            .accounts
            .iter()
            .map(|account| account.username.as_str())
            .collect::<Vec<_>>();
        let names = text_policy::fit_distinct(&names, text_style::NAME, size::ACCOUNT_WIDTH);
        let usernames =
            text_policy::fit_distinct(&usernames, text_style::MESSAGE, size::ACCOUNT_WIDTH);
        let ambiguous = ambiguous_names(&self.accounts);
        let focused = self.focused_account();
        let mut items = self
            .accounts
            .iter()
            .zip(names)
            .zip(usernames)
            .zip(ambiguous)
            .enumerate()
            .map(|(index, (((account, name), username), ambiguous))| {
                account_item(
                    account,
                    name,
                    ambiguous.then_some(username),
                    interactive,
                    focused == Some(index),
                )
            })
            .collect::<Vec<_>>();
        let columns = account_columns(width);
        let mut rows = Vec::new();
        while !items.is_empty() {
            let rest = items.split_off(columns.min(items.len()));
            rows.push(
                row(std::mem::replace(&mut items, rest))
                    .spacing(spacing::ACCOUNT_GAP)
                    .align_y(Alignment::Start)
                    .into(),
            );
        }
        container(
            scrollable(
                column(rows)
                    .width(Fill)
                    .align_x(Alignment::Center)
                    .spacing(spacing::ACCOUNT_GAP),
            )
            .id(self.account_scroll_id.clone())
            .width(Fill)
            .height(Length::Shrink)
            .style(style::scrollbar),
        )
        .width(Fill)
        .height(Fill)
        .align_y(Alignment::End)
        .into()
    }

    fn status_bar(&self, size: Size) -> Element<'_, Message> {
        let open = self.open_menu();
        let mut items = row![].spacing(spacing::BAR_GAP).align_y(Alignment::Center);
        if let Some(session) = &self.selected_session {
            let active = open == Some(Menu::Session);
            let focused = self.is_focused(FocusTarget::Session);
            let name = text_policy::fit(
                &session.name,
                text_style::BAR,
                session_label_width(size.width),
            );
            items = items.push(
                button(
                    row![
                        glyph::glyph(Glyph::Monitor, color(palette::TEXT_PRIMARY)),
                        label(name, text_style::BAR, palette::TEXT_PRIMARY),
                    ]
                    .spacing(component::ITEM_CONTENT_GAP)
                    .align_y(Alignment::Center),
                )
                .on_press_maybe(self.can_select_session().then_some(if active {
                    Message::MenuClosed
                } else {
                    Message::SessionMenuOpened
                }))
                .height(size::BAR_HEIGHT)
                .padding([0.0, component::ITEM_PADDING_X])
                .style(move |_, _| style::status_item(active, focused && !active)),
            );
        }
        let power_active = open == Some(Menu::Power);
        let power_focused = self.is_focused(FocusTarget::Power);
        items = items.push(
            button(
                container(glyph::glyph(Glyph::Power, color(palette::TEXT_PRIMARY)))
                    .height(Fill)
                    .align_y(Alignment::Center),
            )
            .on_press_maybe(self.can_request_power().then_some(if power_active {
                Message::MenuClosed
            } else {
                Message::PowerMenuOpened
            }))
            .height(size::BAR_HEIGHT)
            .padding([0.0, component::ICON_ITEM_PADDING_X])
            .style(move |_, _| style::status_item(power_active, power_focused && !power_active)),
        );

        // A missing session is reported next to where the session item goes.
        let mut bar = column![items]
            .align_x(Alignment::End)
            .spacing(spacing::IDENTITY_GAP);
        if let Some(notice) = self.session_message.as_deref() {
            bar = bar.push(message(notice));
        }
        if self.phase == Phase::Failed && self.selected_session.is_none() {
            bar = bar.push(small_pill(
                "Retry Session Discovery",
                (self.closing.is_none() && self.power_state == PowerState::Idle)
                    .then_some(Message::RetrySession),
                self.is_focused(FocusTarget::RetrySession),
            ));
        }
        container(bar)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .padding(padding::top(spacing::BAR_INSET_TOP).right(spacing::BAR_INSET_RIGHT))
            .into()
    }

    fn preview_badge(&self, size: Size) -> Element<'_, Message> {
        let Some(notice) = self.preview_message.as_deref() else {
            return Space::new().into();
        };
        // The badge mirrors the status bar and keeps to the left half.
        let max_width = (size.width / 2.0 - spacing::BAR_INSET_RIGHT - spacing::BAR_GAP)
            .clamp(0.0, size::MESSAGE_WIDTH);
        let corner = size::BAR_HEIGHT * radius::ROUND;
        container(
            container(
                label(notice, text_style::MESSAGE, palette::TEXT_PRIMARY)
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .padding([
                (size::BAR_HEIGHT - text_style::MESSAGE.line_height) / 2.0,
                component::PILL_PADDING_X,
            ])
            .max_width(max_width)
            .style(move |_| style::badge(corner)),
        )
        .width(Fill)
        .height(Fill)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .padding(padding::top(spacing::BAR_INSET_TOP).left(spacing::BAR_INSET_RIGHT))
        .into()
    }

    /// The open menu, floating under its status item without moving anything.
    fn menu(&self, size: Size) -> Option<Element<'_, Message>> {
        let menu = self.open_menu()?;
        let anchor = menu_anchor(size, menu);
        let content = match menu {
            Menu::Session => self.session_menu(anchor.width),
            Menu::Power => self.power_menu(),
        };
        let surface = container(
            scrollable(content)
                .width(Fill)
                .height(Length::Shrink)
                .style(style::scrollbar),
        )
        .padding(spacing::MENU_PADDING)
        .width(anchor.width)
        .max_height(anchor.max_height)
        .style(style::menu);
        Some(
            stack![
                // Clicking anywhere outside the menu dismisses it.
                opaque(
                    mouse_area(Space::new().width(Fill).height(Fill)).on_press(Message::MenuClosed)
                ),
                pin(surface).x(anchor.x).y(anchor.y),
            ]
            .into(),
        )
    }

    fn session_menu(&self, width: f32) -> Element<'_, Message> {
        let names = self
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>();
        let labels = text_policy::fit_distinct(
            &names,
            text_style::MENU,
            (width
                - 2.0 * spacing::MENU_PADDING
                - component::MENU_ITEM_LEADING
                - component::MENU_ITEM_TRAILING)
                .max(0.0),
        );
        let heading = container(label(
            "Session",
            text_style::MENU_HEADING,
            palette::TEXT_MENU_HEADING,
        ))
        .padding(Padding {
            top: component::MENU_HEADING_PADDING_Y,
            bottom: component::MENU_HEADING_PADDING_Y,
            left: component::MENU_ITEM_LEADING,
            right: component::MENU_ITEM_TRAILING,
        });
        column![heading]
            .extend(
                self.sessions
                    .iter()
                    .zip(labels)
                    .enumerate()
                    .map(|(index, (session, name))| {
                        self.menu_item(
                            index,
                            name,
                            self.selected_session.as_ref() == Some(session),
                            Message::SelectSession(session.clone()),
                        )
                    }),
            )
            .width(Fill)
            .into()
    }

    fn power_menu(&self) -> Element<'_, Message> {
        column(POWER_MENU.iter().enumerate().map(|(index, action)| {
            self.menu_item(
                index,
                power_menu_label(*action),
                false,
                Message::AskPower(*action),
            )
        }))
        .width(Fill)
        .into()
    }

    fn menu_item(
        &self,
        index: usize,
        name: String,
        checked: bool,
        choose: Message,
    ) -> Element<'_, Message> {
        let highlighted = self.menu_highlight == index;
        let check: Element<'_, Message> = if checked {
            glyph::glyph(Glyph::Check, color(palette::TEXT_PRIMARY))
        } else {
            Space::new().into()
        };
        mouse_area(
            button(
                row![
                    container(check)
                        .width(component::MENU_ITEM_LEADING)
                        .padding(padding::left(component::MENU_CHECK_INSET)),
                    label(name, text_style::MENU, palette::TEXT_PRIMARY),
                ]
                .height(Fill)
                .align_y(Alignment::Center),
            )
            .on_press(choose)
            .width(Fill)
            .height(size::ITEM_HEIGHT)
            .padding(padding::right(component::MENU_ITEM_TRAILING))
            .style(move |_, _| style::menu_item(highlighted)),
        )
        .on_enter(Message::MenuHighlighted(index))
        .into()
    }

    /// The Restart or Shut Down alert over the modal scrim.
    fn alert(&self) -> Option<Element<'_, Message>> {
        let (action, executing) = match self.power_state {
            PowerState::Confirming(action) => (action, false),
            PowerState::Executing(action) if action != PowerAction::Suspend => (action, true),
            _ => return None,
        };
        let interactive = self.power_dialog_interactive();
        let icon = container(glyph::sized(
            Glyph::Power,
            color(palette::TEXT_PRIMARY),
            Size::new(component::ALERT_GLYPH, component::ALERT_GLYPH),
        ))
        .width(component::ALERT_ICON)
        .height(component::ALERT_ICON)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(style::alert_icon);
        let mut body = column![
            icon,
            wrapped(
                alert_title(action),
                text_style::ALERT_TITLE,
                palette::TEXT_PRIMARY
            ),
            wrapped(
                POWER_CONSEQUENCE,
                text_style::ALERT_BODY,
                palette::TEXT_SECONDARY
            ),
        ]
        .width(Fill)
        .align_x(Alignment::Center)
        .spacing(component::ALERT_GAP);
        if executing {
            body = body.push(wrapped(
                format!("Requesting {}…", action.label().to_lowercase()),
                text_style::ALERT_BODY,
                palette::TEXT_PRIMARY,
            ));
        }
        let actions = row![
            alert_button(
                "Cancel",
                false,
                self.is_focused(FocusTarget::DialogCancel),
                interactive.then_some(Message::CancelPower),
            ),
            alert_button(
                action.label(),
                true,
                self.is_focused(FocusTarget::DialogConfirm),
                interactive.then_some(Message::ConfirmPower(action)),
            ),
        ]
        .spacing(component::ALERT_ACTIONS_GAP);
        body = body.push(container(actions).padding(padding::top(component::ALERT_ACTIONS_MARGIN)));
        let surface = container(body)
            .width(size::ALERT_WIDTH)
            .padding(Padding {
                top: component::ALERT_PADDING_TOP,
                right: spacing::ALERT_PADDING,
                bottom: spacing::ALERT_PADDING,
                left: spacing::ALERT_PADDING,
            })
            .style(style::alert);
        Some(modal_widget::barrier(stack![
            container(Space::new().width(Fill).height(Fill))
                .width(Fill)
                .height(Fill)
                .style(style::scrim),
            container(surface)
                .width(Fill)
                .height(Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
        ]))
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

/// The height of the identity column without PAM text: avatar, name, pill.
fn identity_height() -> f32 {
    size::AVATAR
        + spacing::IDENTITY_GAP
        + text_style::NAME.line_height
        + spacing::IDENTITY_GAP
        + size::FIELD_HEIGHT
}

/// Scales only the clock down when the region cannot fit it above the
/// identity column, or when the time is wider than the region.
pub(super) fn clock_layout(region: Size, clock_width: f32) -> ClockLayout {
    let natural = spacing::CLOCK_TOP + text_style::DATE.line_height + text_style::CLOCK.line_height;
    let identity_top = region.height - spacing::IDENTITY_BOTTOM - identity_height();
    let by_height = (identity_top - spacing::IDENTITY_GAP) / natural;
    let by_width = if clock_width > 0.0 {
        (region.width - 2.0 * spacing::BAR_INSET_RIGHT) / clock_width
    } else {
        1.0
    };
    let scale = by_height
        .min(by_width)
        .clamp(component::CLOCK_MIN_SCALE, 1.0);
    ClockLayout {
        scale,
        top: spacing::CLOCK_TOP * scale,
        bottom: natural * scale,
    }
}

/// The pill's displacement `elapsed` into a rejection shake: a damped sine
/// that starts and ends at rest.
pub(super) fn shake_offset(elapsed: Duration) -> f32 {
    let progress = (elapsed.as_secs_f32() / motion::SHAKE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    let damping = (1.0 - progress).powi(2);
    motion::SHAKE_AMPLITUDE * damping * (progress * motion::SHAKE_CYCLES * TAU).sin()
}

/// How many accounts fit on one row of the AccountRow.
fn account_columns(width: f32) -> usize {
    let available = width - 2.0 * spacing::BAR_INSET_RIGHT + spacing::ACCOUNT_GAP;
    ((available / (size::ACCOUNT_WIDTH + spacing::ACCOUNT_GAP)) as usize).max(1)
}

/// Whether each account's display name is shared with another account, in
/// which case its username is shown too.
fn ambiguous_names(accounts: &[Account]) -> Vec<bool> {
    let normalize = |name: &str| name.split_whitespace().collect::<Vec<_>>().join(" ");
    accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            accounts.iter().enumerate().any(|(other, candidate)| {
                other != index
                    && normalize(&candidate.display_name) == normalize(&account.display_name)
            })
        })
        .collect()
}

fn power_item_width() -> f32 {
    component::GLYPH_POWER + 2.0 * component::ICON_ITEM_PADDING_X
}

/// The widest the session item's label may be: never wider than a menu, and
/// keeping the status bar to the right half of the region.
fn session_label_width(region_width: f32) -> f32 {
    let half = region_width / 2.0
        - spacing::BAR_INSET_RIGHT
        - power_item_width()
        - spacing::BAR_GAP
        - 2.0 * component::ITEM_PADDING_X
        - component::GLYPH_MONITOR_WIDTH
        - component::ITEM_CONTENT_GAP;
    half.clamp(0.0, size::MENU_WIDTH)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct MenuAnchor {
    x: f32,
    y: f32,
    width: f32,
    max_height: f32,
}

/// A menu hangs under its status item, right-aligned with it.
fn menu_anchor(region: Size, menu: Menu) -> MenuAnchor {
    let item_right = match menu {
        Menu::Power => 0.0,
        Menu::Session => power_item_width() + spacing::BAR_GAP,
    };
    let width = size::MENU_WIDTH
        .min(region.width - 2.0 * spacing::BAR_INSET_RIGHT)
        .max(0.0);
    let x = (region.width - spacing::BAR_INSET_RIGHT - item_right - width).max(0.0);
    let y = spacing::BAR_INSET_TOP + size::BAR_HEIGHT + component::MENU_OFFSET;
    MenuAnchor {
        x,
        y,
        width,
        max_height: (region.height - y - spacing::BAR_INSET_TOP).max(size::ITEM_HEIGHT),
    }
}

fn power_menu_label(action: PowerAction) -> String {
    match action {
        PowerAction::Suspend => action.label().to_owned(),
        // The ellipsis marks the actions that ask for confirmation.
        PowerAction::Reboot | PowerAction::PowerOff => format!("{}…", action.label()),
    }
}

fn alert_title(action: PowerAction) -> &'static str {
    match action {
        PowerAction::Suspend => "Are you sure you want to sleep your computer now?",
        PowerAction::Reboot => "Are you sure you want to restart your computer now?",
        PowerAction::PowerOff => "Are you sure you want to shut down your computer now?",
    }
}

fn authentication_region_or_full(
    region: Option<crate::outputs::Region>,
    size: Size,
) -> crate::outputs::Region {
    region.unwrap_or(crate::outputs::Region {
        x: 0.0,
        y: 0.0,
        width: size.width,
        height: size.height,
        layout_width: size.width.max(1.0),
        layout_height: size.height.max(1.0),
    })
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

/// Centered text that wraps within its container.
fn wrapped<'a>(
    content: impl iced::widget::text::IntoFragment<'a>,
    style: TextStyle,
    tone: Rgba,
) -> Element<'a, Message> {
    label(content, style, tone)
        .wrapping(Wrapping::WordOrGlyph)
        .width(Fill)
        .align_x(Alignment::Center)
        .into()
}

/// A StatusMessage: PAM text centered and wrapped at `message-width`.
fn message<'a>(content: &'a str) -> Element<'a, Message> {
    container(
        label(content, text_style::MESSAGE, palette::TEXT_PRIMARY)
            .wrapping(Wrapping::WordOrGlyph)
            .align_x(Alignment::Center),
    )
    .max_width(size::MESSAGE_WIDTH)
    .into()
}

fn avatar<'a>(name: &str) -> Element<'a, Message> {
    container(label(
        initials(name),
        text_style::INITIALS,
        palette::TEXT_PRIMARY,
    ))
    .width(size::AVATAR)
    .height(size::AVATAR)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .style(|_| style::avatar(size::AVATAR))
    .into()
}

fn account_item<'a>(
    account: &'a Account,
    name: String,
    username: Option<String>,
    interactive: bool,
    focused: bool,
) -> Element<'a, Message> {
    let ring = size::AVATAR + 2.0 * component::FOCUS_RING_WIDTH;
    let avatar = container(avatar(&account.display_name))
        .width(ring)
        .height(ring)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_| style::avatar_focus(ring, focused));
    let mut names =
        column![label(name, text_style::NAME, palette::TEXT_PRIMARY)].align_x(Alignment::Center);
    if let Some(username) = username {
        names = names.push(label(
            username,
            text_style::MESSAGE,
            palette::TEXT_SECONDARY,
        ));
    }
    tile_widget::tile(
        column![avatar, names]
            .width(Fill)
            .align_x(Alignment::Center)
            .spacing(spacing::IDENTITY_GAP),
        interactive.then(|| Message::SelectAccount(account.clone())),
        size::ACCOUNT_WIDTH,
        tile_widget::id(&account.username),
    )
}

/// A small pill button for recovery actions.
fn small_pill<'a>(
    content: &'a str,
    on_press: Option<Message>,
    focused: bool,
) -> Element<'a, Message> {
    let tone = if on_press.is_some() {
        palette::TEXT_PRIMARY
    } else {
        palette::TEXT_SECONDARY
    };
    button(
        container(label(content, text_style::BAR, tone))
            .height(Fill)
            .align_y(Alignment::Center),
    )
    .on_press_maybe(on_press)
    .height(size::PILL_HEIGHT)
    .padding([0.0, component::PILL_PADDING_X])
    .style(move |_, status| style::pill(false, focused, status))
    .into()
}

/// One of an alert's two equal-width pill buttons.
fn alert_button<'a>(
    content: &'a str,
    default: bool,
    focused: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let tone = if on_press.is_some() {
        palette::TEXT_PRIMARY
    } else {
        palette::TEXT_SECONDARY
    };
    button(
        container(label(content, text_style::BAR, tone))
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    )
    .on_press_maybe(on_press)
    .width(Fill)
    .height(size::PILL_HEIGHT)
    .padding(0)
    .style(move |_, status| style::pill(default, focused, status))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_output_geometry_uses_the_full_window_positioning_region() {
        let size = Size::new(800.0, 600.0);
        let region = authentication_region_or_full(None, size);

        assert_eq!(region.x, 0.0);
        assert_eq!(region.y, 0.0);
        assert_eq!(region.width, size.width);
        assert_eq!(region.height, size.height);
        assert_eq!(region.scale_to(size.width, size.height), region);
    }

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
    fn the_clock_keeps_its_token_size_when_the_region_has_room() {
        let layout = clock_layout(Size::new(1280.0, 800.0), 600.0);
        assert_eq!(layout.scale, 1.0);
        assert_eq!(layout.top, spacing::CLOCK_TOP);
        let identity_top = 800.0 - spacing::IDENTITY_BOTTOM - identity_height();
        assert!(layout.bottom + spacing::IDENTITY_GAP <= identity_top);
    }

    #[test]
    fn only_the_clock_shrinks_in_short_or_narrow_regions() {
        for region in [Size::new(480.0, 600.0), Size::new(1280.0, 560.0)] {
            let layout = clock_layout(region, 600.0);
            let identity_top = region.height - spacing::IDENTITY_BOTTOM - identity_height();
            assert!(layout.scale < 1.0, "{region:?}");
            assert!(
                layout.bottom + spacing::IDENTITY_GAP <= identity_top + 0.01,
                "{region:?}: clock ends at {} but identity starts at {identity_top}",
                layout.bottom
            );
        }
        let narrow = clock_layout(Size::new(400.0, 1600.0), 600.0);
        assert!((narrow.scale - (400.0 - 2.0 * spacing::BAR_INSET_RIGHT) / 600.0).abs() < 1e-4);
        assert_eq!(
            clock_layout(Size::new(100.0, 100.0), 600.0).scale,
            component::CLOCK_MIN_SCALE
        );
    }

    #[test]
    fn the_shake_is_a_damped_oscillation_that_ends_at_rest() {
        assert_eq!(shake_offset(Duration::ZERO), 0.0);
        assert!(shake_offset(motion::SHAKE_DURATION).abs() < 1e-4);
        assert!(shake_offset(motion::SHAKE_DURATION * 2).abs() < 1e-4);
        let samples = (0..=40)
            .map(|step| shake_offset(motion::SHAKE_DURATION * step / 40))
            .collect::<Vec<_>>();
        let peak = samples
            .iter()
            .fold(0.0f32, |peak, offset| peak.max(offset.abs()));
        assert!(peak > motion::SHAKE_AMPLITUDE * 0.5 && peak <= motion::SHAKE_AMPLITUDE);
        assert!(samples.iter().any(|offset| *offset < 0.0));
        assert!(samples.iter().any(|offset| *offset > 0.0));
        let early = samples[..20]
            .iter()
            .fold(0.0f32, |peak, offset| peak.max(offset.abs()));
        let late = samples[20..]
            .iter()
            .fold(0.0f32, |peak, offset| peak.max(offset.abs()));
        assert!(late < early, "the oscillation is damped");
    }

    #[test]
    fn account_rows_wrap_to_the_region_width() {
        assert_eq!(account_columns(200.0), 1);
        assert_eq!(account_columns(480.0), 2);
        assert_eq!(account_columns(1280.0), 8);
    }

    #[test]
    fn usernames_are_shown_only_for_duplicate_display_names() {
        let accounts = [
            Account::override_account("alex".into(), Some("Alex Morgan".into())),
            Account::override_account("amorgan".into(), Some("Alex  Morgan".into())),
            Account::override_account("bob".into(), Some("Bob".into())),
        ];
        assert_eq!(ambiguous_names(&accounts), vec![true, true, false]);
    }

    #[test]
    fn the_status_bar_and_menus_stay_inside_the_region() {
        assert_eq!(session_label_width(3008.0), size::MENU_WIDTH);
        assert!(session_label_width(480.0) < size::MENU_WIDTH);
        assert_eq!(session_label_width(100.0), 0.0);

        let region = Size::new(1280.0, 800.0);
        let power = menu_anchor(region, Menu::Power);
        assert_eq!(power.width, size::MENU_WIDTH);
        assert_eq!(
            power.x + power.width,
            region.width - spacing::BAR_INSET_RIGHT
        );
        assert_eq!(
            power.y,
            spacing::BAR_INSET_TOP + size::BAR_HEIGHT + component::MENU_OFFSET
        );
        let session = menu_anchor(region, Menu::Session);
        assert!(session.x < power.x);

        let narrow = menu_anchor(Size::new(200.0, 300.0), Menu::Session);
        assert!(narrow.x >= 0.0);
        assert!(narrow.width <= 200.0);
    }

    #[test]
    fn power_menu_marks_confirmed_actions_with_an_ellipsis() {
        assert_eq!(
            POWER_MENU.map(power_menu_label),
            ["Sleep".to_owned(), "Restart…".into(), "Shut Down…".into()]
        );
        assert_eq!(
            alert_title(PowerAction::Reboot),
            "Are you sure you want to restart your computer now?"
        );
        assert_eq!(
            alert_title(PowerAction::PowerOff),
            "Are you sure you want to shut down your computer now?"
        );
    }
}
