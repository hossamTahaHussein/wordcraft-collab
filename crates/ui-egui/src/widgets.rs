//! Ribbon widgets: large and small buttons, split buttons, groups, combo fields, colour grids.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use serde_json::Value;

use crate::theme::{Tokens, regular};
use crate::{WordApp, icons};

mod buttons;
mod combo;
mod groups;
mod icon_buttons;
mod tooltip;

pub use buttons::{
    BIG_ICON_Y, BIG_LABEL_Y, ButtonAction, CONTENT_H, DROPDOWN, LABEL_H, LABEL_LINE_H, LABEL_PITCH, MenuButtonOptions, big, big_toggle, enabled,
    label_line, menu_button, menu_enabled, small, split,
};
pub(crate) use buttons::{bg, run_from_ribbon};
pub(crate) use combo::swatch;
pub use combo::{
    ADD_FONT, COMBO_PREVIEW_ROW_H, DrawnState, FontComboOptions, LOAD_SYSTEM_FONTS, color_grid, combo, combo_hint, combo_list, drawn_state,
    font_combo, font_combo_with_recent, font_combo_with_recent_hint, font_matches,
};
#[cfg(test)]
use combo::{Row, highlight_row, section_row, section_rows};
pub use groups::{command_icon, command_menu, customize_menu_items, group, group_titled, row};
pub use icon_buttons::{
    dialog_tabs, icon_button, icon_menu_item, icon_only_button, icon_text_button, icon_text_button_width, icon_text_menu_button, primary_button,
};
#[cfg(test)]
use tooltip::show_hint;
#[cfg(test)]
pub(crate) use tooltip::tooltip_lines;
pub use tooltip::{key_text, shortcut_text};
// #653: a two-line label ends inside the content, above the group title row.
const _: () = assert!(BIG_LABEL_Y + LABEL_PITCH + LABEL_LINE_H <= CONTENT_H);
const _: () = assert!(BIG_ICON_Y + 16.0 <= BIG_LABEL_Y);

/// A top-aligned row in reading order (a ribbon group's controls, a gallery's thumbnails).
pub(crate) fn row_top(ui: &Ui) -> egui::Layout {
    if craft_ui::direction::is_rtl(ui) { egui::Layout::right_to_left(egui::Align::Min) } else { egui::Layout::left_to_right(egui::Align::Min) }
}

/// A top-aligned row pinned to the reading end (a pane header's close button):
/// right to left in a left-to-right interface, left to right in a right-to-left one.
pub(crate) fn trailing_top(ui: &Ui) -> egui::Layout {
    if craft_ui::direction::is_rtl(ui) { egui::Layout::left_to_right(egui::Align::Min) } else { egui::Layout::right_to_left(egui::Align::Min) }
}

/// [`Ui::vertical`] against the reading start: a stack of controls in a mirrored row (a ribbon
/// group's columns) sits against the row's cursor, not at the far side of the space left.
pub(crate) fn column<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let layout = craft_ui::direction::column(ui);
    ui.scope_builder(egui::UiBuilder::new().layout(layout), add)
}

#[cfg(test)]
thread_local! {
    /// Groups drawn taller than [`CONTENT_H`] (title and height): their content runs into the
    /// group label below, as View › Show did with four check boxes in one column.
    pub(crate) static TALL_GROUPS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Each group's dialog launcher as drawn: the group's title, the launcher's rect and the
    /// title row's.
    pub(crate) static LAUNCHERS: std::cell::RefCell<Vec<(String, egui::Rect, egui::Rect)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
mod tests;
