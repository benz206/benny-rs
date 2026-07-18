use super::model::EmbedData;
use super::{
    BTN_ADDFIELD, BTN_AUTHOR, BTN_BACK, BTN_BASE, BTN_CANCEL, BTN_COMPLETE, BTN_EXPORT_JSON,
    BTN_EXPORT_MYST, BTN_FOOTER, BTN_IMAGES, BTN_IMPORT, BTN_REMOVEFIELD, BTN_SEND, MAX_FIELDS,
    MODAL_ADDFIELD, MODAL_AUTHOR, MODAL_BASE, MODAL_FOOTER, MODAL_IMAGES, MODAL_IMPORT,
    SEL_REMOVE, SEL_SEND,
};
use crate::utils::format::truncate;
use serenity::all::{
    ButtonStyle, ChannelType, CreateActionRow, CreateButton, CreateInputText, CreateModal,
    CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, InputTextStyle,
};

// ---- free helpers: components ---------------------------------------------

/// The main builder control layout (4 rows of buttons).
pub(super) fn build_main_components(data: &EmbedData) -> Vec<CreateActionRow> {
    let edit_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_AUTHOR)
            .label("Author")
            .style(ButtonStyle::Primary)
            .emoji('📝'),
        CreateButton::new(BTN_BASE)
            .label("Base")
            .style(ButtonStyle::Primary)
            .emoji('🗒'),
        CreateButton::new(BTN_IMAGES)
            .label("Images")
            .style(ButtonStyle::Primary)
            .emoji('🖼'),
        CreateButton::new(BTN_FOOTER)
            .label("Footer")
            .style(ButtonStyle::Primary)
            .emoji('📜'),
    ]);
    let field_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_ADDFIELD)
            .label("Add Field")
            .style(ButtonStyle::Success)
            .emoji('➕'),
        CreateButton::new(BTN_REMOVEFIELD)
            .label("Remove Field")
            .style(ButtonStyle::Danger)
            .emoji('➖')
            .disabled(data.fields.is_empty()),
        CreateButton::new(BTN_IMPORT)
            .label("Import")
            .style(ButtonStyle::Secondary)
            .emoji('📥'),
    ]);
    let io_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_SEND)
            .label("Send")
            .style(ButtonStyle::Success)
            .emoji('💬'),
        CreateButton::new(BTN_EXPORT_JSON)
            .label("Export JSON")
            .style(ButtonStyle::Secondary)
            .emoji('📤'),
        CreateButton::new(BTN_EXPORT_MYST)
            .label("Export to Mystbin")
            .style(ButtonStyle::Secondary)
            .emoji('🗄'),
    ]);
    let finish_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_COMPLETE)
            .label("Complete")
            .style(ButtonStyle::Success)
            .emoji('✅'),
        CreateButton::new(BTN_CANCEL)
            .label("Cancel")
            .style(ButtonStyle::Danger)
            .emoji('❌'),
    ]);
    vec![edit_row, field_row, io_row, finish_row]
}

/// The "remove a field" sub-view: a select of the existing fields + Back.
pub(super) fn build_remove_components(data: &EmbedData) -> Vec<CreateActionRow> {
    let options: Vec<CreateSelectMenuOption> = data
        .fields
        .iter()
        .take(MAX_FIELDS)
        .enumerate()
        .map(|(i, f)| {
            let name = if f.name.trim().is_empty() || f.name == "\u{200b}" {
                "(no name)".to_string()
            } else {
                truncate(&f.name, 90).to_string()
            };
            CreateSelectMenuOption::new(format!("Field {}: {}", i + 1, name), i.to_string())
                .emoji('🗑')
        })
        .collect();
    vec![
        CreateActionRow::SelectMenu(
            CreateSelectMenu::new(SEL_REMOVE, CreateSelectMenuKind::String { options })
                .placeholder("Select a field to remove")
                .min_values(1)
                .max_values(1),
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(BTN_BACK)
                .label("Back")
                .style(ButtonStyle::Secondary)
                .emoji('↩'),
        ]),
    ]
}

/// The "send to a channel" sub-view: a channel select + Back.
pub(super) fn build_send_components() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::SelectMenu(
            CreateSelectMenu::new(
                SEL_SEND,
                CreateSelectMenuKind::Channel {
                    channel_types: Some(vec![ChannelType::Text, ChannelType::News]),
                    default_channels: None,
                },
            )
            .placeholder("Select a channel to send to")
            .min_values(1)
            .max_values(1),
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(BTN_BACK)
                .label("Back")
                .style(ButtonStyle::Secondary)
                .emoji('↩'),
        ]),
    ]
}

// ---- free helpers: modals -------------------------------------------------

fn short(custom_id: &str, label: &str, max: u16) -> CreateInputText {
    CreateInputText::new(InputTextStyle::Short, label, custom_id)
        .required(false)
        .max_length(max)
}

fn paragraph(custom_id: &str, label: &str, max: u16) -> CreateInputText {
    CreateInputText::new(InputTextStyle::Paragraph, label, custom_id)
        .required(false)
        .max_length(max)
}

/// Apply a pre-fill value when present.
fn prefill(input: CreateInputText, value: &Option<String>) -> CreateInputText {
    match value {
        Some(v) if !v.is_empty() => input.value(v.clone()),
        _ => input,
    }
}

pub(super) fn build_author_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("name", "Name", 256).placeholder("Author name"),
            &data.author_name,
        )),
        CreateActionRow::InputText(prefill(
            short("author_url", "URL", 1024).placeholder("Author URL (optional)"),
            &data.author_url,
        )),
        CreateActionRow::InputText(prefill(
            short("author_icon", "Icon URL", 1024).placeholder("Author icon URL (optional)"),
            &data.author_icon,
        )),
    ];
    CreateModal::new(MODAL_AUTHOR, "Edit Author").components(rows)
}

pub(super) fn build_base_modal(data: &EmbedData) -> CreateModal {
    let color_value = data.color.map(|c| format!("#{c:06X}"));
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("title", "Title", 256).placeholder("Title"),
            &data.title,
        )),
        CreateActionRow::InputText(prefill(
            paragraph("description", "Description", 4000).placeholder("Description (optional)"),
            &data.description,
        )),
        CreateActionRow::InputText(prefill(
            short("color", "Color", 7).placeholder("#5865F2 (optional)"),
            &color_value,
        )),
        CreateActionRow::InputText(prefill(
            short("url", "Title URL", 1024).placeholder("Title URL (optional)"),
            &data.url,
        )),
    ];
    CreateModal::new(MODAL_BASE, "Edit Base").components(rows)
}

pub(super) fn build_images_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("image_url", "Image URL", 1024).placeholder("Image URL (optional)"),
            &data.image_url,
        )),
        CreateActionRow::InputText(prefill(
            short("thumbnail_url", "Thumbnail URL", 1024).placeholder("Thumbnail URL (optional)"),
            &data.thumbnail_url,
        )),
    ];
    CreateModal::new(MODAL_IMAGES, "Edit Images").components(rows)
}

pub(super) fn build_footer_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            paragraph("text", "Text", 2048).placeholder("Footer text"),
            &data.footer_text,
        )),
        CreateActionRow::InputText(prefill(
            short("footer_icon", "Icon URL", 1024).placeholder("Footer icon URL (optional)"),
            &data.footer_icon,
        )),
    ];
    CreateModal::new(MODAL_FOOTER, "Edit Footer").components(rows)
}

pub(super) fn build_addfield_modal() -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(short("field_name", "Name", 256).placeholder("Field name")),
        CreateActionRow::InputText(
            paragraph("field_value", "Value", 1024).placeholder("Field value"),
        ),
        CreateActionRow::InputText(short("field_inline", "Inline", 5).placeholder("true / false")),
    ];
    CreateModal::new(MODAL_ADDFIELD, "Add Field").components(rows)
}

pub(super) fn build_import_modal() -> CreateModal {
    let rows = vec![CreateActionRow::InputText(
        paragraph("import_link", "JSON or Mystbin link", 4000)
            .placeholder("https://mystb.in/SomeID or raw embed JSON"),
    )];
    CreateModal::new(MODAL_IMPORT, "Import Embed").components(rows)
}
