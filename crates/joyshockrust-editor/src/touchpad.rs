use std::{cell::Cell as State, rc::Rc, time::Duration};

use adw::{gtk, prelude::*};
use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value, value};

use crate::{
    bindings::{self, Gesture, region_input, replace_value},
    controller, controller_art, controller_input,
    ui::{self, Context},
};

const MAX_CELLS: i64 = 25;
const ADVANCED: &str = "Edit this touchpad configuration in Advanced TOML.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    row: i64,
    column: i64,
}

impl Cell {
    fn read(table: &dyn TableLike) -> Result<Self, &'static str> {
        let coordinate = |name| table.get(name).and_then(Item::as_integer).ok_or(ADVANCED);
        Ok(Self {
            row: coordinate("row")?,
            column: coordinate("column")?,
        })
    }

    fn in_grid(self, size: Self) -> bool {
        (1..=size.row).contains(&self.row) && (1..=size.column).contains(&self.column)
    }

    fn input(self, kind: &str) -> String {
        format!("{kind}:{}:{}", self.row, self.column)
    }

    fn from_input(input: &str) -> Option<Self> {
        let (_, row, column) = region_input(input)?;
        Some(Self { row, column })
    }
}

const DEFAULT_SIZE: Cell = Cell { row: 1, column: 2 };
const FIRST_CELL: Cell = Cell { row: 1, column: 1 };

fn grid(doc: &DocumentMut) -> Result<Cell, &'static str> {
    let Some(table) = doc.get("touchpad") else {
        return Ok(DEFAULT_SIZE);
    };
    let table = table.as_table().ok_or(ADVANCED)?;
    let rows = table
        .get("rows")
        .and_then(Item::as_integer)
        .ok_or(ADVANCED)?;
    let columns = table
        .get("columns")
        .and_then(Item::as_integer)
        .ok_or(ADVANCED)?;
    let size = Cell {
        row: rows,
        column: columns,
    };
    validate_size(size)?;
    for region in regions(doc)? {
        if !Cell::read(region)?.in_grid(size) {
            return Err(ADVANCED);
        }
    }
    Ok(size)
}

fn validate_size(size: Cell) -> Result<(), &'static str> {
    if size.row < 1 || size.column < 1 || size.row.saturating_mul(size.column) > MAX_CELLS {
        return Err("The touchpad grid must have between 1 and 25 cells.");
    }
    Ok(())
}

fn regions(doc: &DocumentMut) -> Result<impl Iterator<Item = &Table>, &'static str> {
    let regions = doc
        .get("touchpad")
        .and_then(|table| table.get("regions"))
        .map(|item| item.as_array_of_tables().ok_or(ADVANCED))
        .transpose()?;
    Ok(regions.into_iter().flat_map(ArrayOfTables::iter))
}

fn nested<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(doc.get(first)?, |item, key| item.get(key))
}

fn binding_uses_removed_cell(doc: &DocumentMut, size: Cell) -> bool {
    let removed = |input: &str| Cell::from_input(input).is_some_and(|cell| !cell.in_grid(size));
    if nested(doc, &["gyro", "binding", "input"])
        .and_then(Item::as_str)
        .is_some_and(|input| input.split(',').any(removed))
    {
        return true;
    }
    let Some(bindings) = nested(doc, &["keyboard", "bindings"]) else {
        return false;
    };
    let tables = bindings
        .as_array_of_tables()
        .into_iter()
        .flat_map(ArrayOfTables::iter)
        .map(|table| table as &dyn TableLike);
    let inline = bindings
        .as_array()
        .into_iter()
        .flat_map(|array| array.iter())
        .filter_map(Value::as_inline_table)
        .map(|table| table as &dyn TableLike);
    tables.chain(inline).any(|table| {
        table
            .get("button")
            .and_then(Item::as_str)
            .is_some_and(removed)
            || table
                .get("chord")
                .and_then(Item::as_array)
                .is_some_and(|chord| chord.iter().filter_map(Value::as_str).any(removed))
    })
}

fn resize(doc: &mut DocumentMut, size: Cell) -> Result<(), &'static str> {
    validate_size(size)?;
    let current = grid(doc)?;
    if doc.contains_key("touchpad") && current == size {
        return Ok(());
    }
    for region in regions(doc)? {
        if !Cell::read(region)?.in_grid(size) {
            return Err("Clear the gamepad actions in the cells outside the new grid first.");
        }
    }
    if binding_uses_removed_cell(doc, size) {
        return Err("Update keyboard or gyro inputs outside the new grid in Advanced TOML first.");
    }
    let table = doc
        .entry("touchpad")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or(ADVANCED)?;
    for (key, number) in [("rows", size.row), ("columns", size.column)] {
        if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
            if old.as_integer() != Some(number) {
                replace_value(old, Value::from(number));
            }
        } else {
            table.insert(key, value(number));
        }
    }
    Ok(())
}

fn region_index(doc: &DocumentMut, cell: Cell) -> Result<Option<usize>, &'static str> {
    let mut matches = regions(doc)?
        .enumerate()
        .filter(|(_, table)| Cell::read(*table) == Ok(cell));
    let index = matches.next().map(|(index, _)| index);
    if matches.next().is_some() {
        return Err(ADVANCED);
    }
    Ok(index)
}

fn set_output(
    doc: &mut DocumentMut,
    cell: Cell,
    field: &str,
    target: Option<&str>,
) -> Result<(), &'static str> {
    let size = grid(doc)?;
    if !cell.in_grid(size) {
        return Err(ADVANCED);
    }
    let index = region_index(doc, cell)?;
    if index.is_none() && target.is_none() {
        return Ok(());
    }
    resize(doc, size)?;
    let regions = doc["touchpad"]
        .as_table_mut()
        .unwrap()
        .entry("regions")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .unwrap();
    match (index, target) {
        (Some(index), Some(target)) => {
            let table = regions.get_mut(index).unwrap();
            if let Some(existing) = table.get_mut(field).and_then(Item::as_value_mut) {
                replace_value(existing, Value::from(target));
            } else {
                table.insert(field, value(target));
            }
        }
        (Some(index), None) => {
            let table = regions.get_mut(index).unwrap();
            table.remove(field);
            if !table.contains_key("touch") && !table.contains_key("click") {
                regions.remove(index);
            }
        }
        (None, Some(target)) => {
            let mut table = Table::new();
            table.insert("row", value(cell.row));
            table.insert("column", value(cell.column));
            table.insert(field, value(target));
            regions.push(table);
        }
        (None, None) => {}
    }
    Ok(())
}

pub fn gyro_inputs(doc: &DocumentMut) -> impl Iterator<Item = (String, String)> {
    grid(doc).ok().into_iter().flat_map(|size| {
        (1..=size.row).flat_map(move |row| {
            (1..=size.column).map(move |column| {
                (
                    Cell { row, column }.input("touch"),
                    format!("Touch region {row}:{column}"),
                )
            })
        })
    })
}

pub fn prepare_gyro_input(doc: &mut DocumentMut, input: &str) -> Result<(), &'static str> {
    let size = grid(doc)?;
    if !Cell::from_input(input).is_some_and(|cell| cell.in_grid(size)) {
        return Err("Choose a touch region inside the configured grid.");
    }
    resize(doc, size)
}

fn set_keyboard_action(
    doc: &mut DocumentMut,
    input: &str,
    gesture: Gesture,
    output: Option<&str>,
) -> Result<(), &'static str> {
    let size = grid(doc)?;
    if output.is_some() {
        resize(doc, size)?;
    }
    bindings::set_key(doc, input, gesture, output)
}

pub fn page(ctx: &Context) -> gtk::Widget {
    let Ok(doc) = ctx.document() else {
        return ui::advanced_required(
            "Fix the profile syntax in Advanced TOML to edit the touchpad.",
        );
    };
    let size = match grid(&doc) {
        Ok(size) => size,
        Err(error) => return ui::advanced_required(error),
    };
    let page = adw::PreferencesPage::new();
    let dimensions = adw::PreferencesGroup::builder()
        .title("Touchpad grid")
        .description("Split the touchpad into up to 25 cells. Select a cell to assign its actions.")
        .build();
    let rows = dimension_row("Rows", size.row);
    let columns = dimension_row("Columns", size.column);
    dimensions.add(&rows);
    dimensions.add(&columns);
    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    dimensions.add(&error);
    page.add(&dimensions);

    let cells = adw::PreferencesGroup::new();
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let selectors = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let selected = Rc::new(State::new(FIRST_CELL));
    let grid_size = Rc::new(State::new(size));
    let preview = live_preview(grid_size.clone(), selected.clone(), &layout);
    layout.append(&selectors);
    cells.add(&layout);
    page.add(&cells);
    cells.add(&actions);
    show_cells(ctx, size, &selectors, &actions, &selected, &preview);

    let ctx = ctx.clone();
    let selectors = selectors.downgrade();
    let actions = actions.downgrade();
    let preview = preview.downgrade();
    let weak_rows = rows.downgrade();
    let weak_columns = columns.downgrade();
    let update = move |_: &adw::SpinRow| {
        let (Some(rows), Some(columns)) = (weak_rows.upgrade(), weak_columns.upgrade()) else {
            return;
        };
        let size = Cell {
            row: rows.value() as i64,
            column: columns.value() as i64,
        };
        error.set_visible(false);
        if size == grid_size.get() {
            return;
        }
        if let Err(message) = ctx.try_edit(|doc| resize(doc, size)) {
            error.set_text(&message);
            error.set_visible(true);
            return;
        }
        grid_size.set(size);
        if !selected.get().in_grid(size) {
            selected.set(FIRST_CELL);
        }
        if let (Some(selectors), Some(actions), Some(preview)) =
            (selectors.upgrade(), actions.upgrade(), preview.upgrade())
        {
            show_cells(&ctx, size, &selectors, &actions, &selected, &preview);
            preview.queue_draw();
        }
    };
    rows.connect_value_notify(update.clone());
    columns.connect_value_notify(update);
    page.upcast()
}

fn dimension_row(title: &str, initial: i64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(1.0, MAX_CELLS as f64, 1.0);
    row.set_title(title);
    row.set_snap_to_ticks(true);
    row.set_value(initial as f64);
    row
}

fn show_cells(
    ctx: &Context,
    size: Cell,
    layout: &gtk::Box,
    actions: &gtk::Box,
    selected: &Rc<State<Cell>>,
    preview: &gtk::DrawingArea,
) {
    while let Some(child) = layout.first_child() {
        layout.remove(&child);
    }
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&grid)
        .build();
    layout.append(&scroll);
    let mut first = None::<gtk::ToggleButton>;
    for row in 1..=size.row {
        for column in 1..=size.column {
            let cell = Cell { row, column };
            let label = format!("Row {row}, column {column}");
            let button = gtk::ToggleButton::with_label(&format!("{row}:{column}"));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            button.set_tooltip_text(Some(&label));
            if let Some(first) = &first {
                button.set_group(Some(first));
            } else {
                first = Some(button.clone());
            }
            button.set_active(cell == selected.get());
            let ctx = ctx.clone();
            let actions = actions.downgrade();
            let selected = selected.clone();
            let preview = preview.downgrade();
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(actions) = actions.upgrade()
                {
                    selected.set(cell);
                    if let Some(preview) = preview.upgrade() {
                        preview.queue_draw();
                    }
                    show_actions(&ctx, cell, &actions);
                }
            });
            grid.attach(&button, column as i32 - 1, row as i32 - 1, 1, 1);
        }
    }
    show_actions(ctx, selected.get(), actions);
}

type Contacts = [Option<(f64, f64)>; 2];

fn parse_contacts(value: &str) -> Option<Contacts> {
    let mut contacts = [None; 2];
    if !value.is_empty() {
        for (index, point) in value.split(';').enumerate() {
            let (x, y) = point.split_once(',')?;
            let x = x.parse::<u16>().ok()?;
            let y = y.parse::<u16>().ok()?;
            if x >= DUALSENSE_TOUCHPAD_WIDTH || y >= DUALSENSE_TOUCHPAD_HEIGHT {
                return None;
            }
            *contacts.get_mut(index)? = Some((
                f64::from(x) / f64::from(DUALSENSE_TOUCHPAD_WIDTH),
                f64::from(y) / f64::from(DUALSENSE_TOUCHPAD_HEIGHT),
            ));
        }
    }
    Some(contacts)
}

fn live_preview(
    size: Rc<State<Cell>>,
    selected: Rc<State<Cell>>,
    layout: &gtk::Box,
) -> gtk::DrawingArea {
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 6);
    preview.set_margin_bottom(12);
    let area = gtk::DrawingArea::builder().content_height(194).build();
    area.update_property(&[gtk::accessible::Property::Label("Live touchpad preview")]);
    preview.append(&area);
    let status = gtk::Label::builder()
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    preview.append(&status);
    layout.append(&preview);

    let contacts = Rc::new(State::new(Contacts::default()));
    let drawn_contacts = contacts.clone();
    let svg = controller_art::load()
        .map_err(|error| {
            eprintln!("DualSense art unavailable: {error}");
        })
        .ok();
    area.set_draw_func(move |area, cr, width, height| {
        draw_preview(
            area,
            cr,
            (width, height),
            size.get(),
            selected.get(),
            drawn_contacts.get(),
            svg.as_ref(),
        );
    });
    let chart = area.clone();
    controller_input::watch_status(
        &preview,
        "touch_contacts=",
        Duration::from_millis(33),
        move |result| {
            let (points, message) = match result {
                Ok(value) => match value.as_deref().and_then(parse_contacts) {
                    Some(points) => (points, ""),
                    None => (Contacts::default(), "Waiting for touchpad reports…"),
                },
                Err(()) => (Contacts::default(), "Bridge status is unavailable."),
            };
            if contacts.replace(points) != points {
                chart.queue_draw();
            }
            status.set_text(message);
        },
    );
    area
}

const CROP_TOP: f64 = 56.0;
const CROP_HEIGHT: f64 = 80.0;
// SVG viewBox coordinates for the touch surface.
const PAD_CENTER: f64 = controller_art::SIZE / 2.0;
const PAD_TOP: f64 = 72.0;
const PAD_BOTTOM: f64 = 118.0;
const PAD_TOP_WIDTH: f64 = 68.0;
const PAD_BOTTOM_WIDTH: f64 = 54.2;

fn touch_position(x: f64, y: f64) -> (f64, f64) {
    let width = PAD_TOP_WIDTH + y * (PAD_BOTTOM_WIDTH - PAD_TOP_WIDTH);
    (
        PAD_CENTER + (x - 0.5) * width,
        PAD_TOP + y * (PAD_BOTTOM - PAD_TOP),
    )
}

fn draw_preview(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    (width, height): (i32, i32),
    size: Cell,
    selected: Cell,
    contacts: Contacts,
    svg: Option<&gtk::gdk_pixbuf::Pixbuf>,
) {
    let style = area.style_context();
    let fg = style.color();
    let accent = style.lookup_color("accent_color").unwrap_or(fg);
    let color = |c: gtk::gdk::RGBA, alpha| {
        cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), alpha);
    };
    let scale = ((f64::from(width) - 12.0) / controller_art::SIZE)
        .min((f64::from(height) - 12.0) / CROP_HEIGHT);
    if scale <= 0.0 {
        return;
    }
    let _ = cr.save();
    cr.translate(
        (f64::from(width) - controller_art::SIZE * scale) / 2.0,
        (f64::from(height) - CROP_HEIGHT * scale) / 2.0,
    );
    cr.scale(scale, scale);
    cr.rectangle(0.0, 0.0, controller_art::SIZE, CROP_HEIGHT);
    cr.clip();
    cr.translate(0.0, -CROP_TOP);

    let _ = cr.save();
    // Clip the overlays using the outline's touchpad path.
    cr.move_to(94.0, PAD_TOP);
    cr.line_to(100.0, 112.0);
    cr.curve_to(101.0, 116.0, 104.0, PAD_BOTTOM, 108.0, PAD_BOTTOM);
    cr.line_to(148.0, PAD_BOTTOM);
    cr.curve_to(152.0, PAD_BOTTOM, 155.0, 116.0, 156.0, 112.0);
    cr.line_to(162.0, PAD_TOP);
    cr.close_path();
    color(fg, 0.09);
    let _ = cr.fill_preserve();
    cr.clip();

    // Zone edges and contact dots share the same surface projection.
    let move_to = |x, y| {
        let (x, y) = touch_position(x, y);
        cr.move_to(x, y);
    };
    let line_to = |x, y| {
        let (x, y) = touch_position(x, y);
        cr.line_to(x, y);
    };
    let left = (selected.column - 1) as f64 / size.column as f64;
    let right = selected.column as f64 / size.column as f64;
    let top = (selected.row - 1) as f64 / size.row as f64;
    let bottom = selected.row as f64 / size.row as f64;
    move_to(left, top);
    line_to(right, top);
    line_to(right, bottom);
    line_to(left, bottom);
    cr.close_path();
    color(accent, 0.28);
    let _ = cr.fill();
    for column in 1..size.column {
        let x = column as f64 / size.column as f64;
        move_to(x, 0.0);
        line_to(x, 1.0);
    }
    for row in 1..size.row {
        let y = row as f64 / size.row as f64;
        move_to(0.0, y);
        line_to(1.0, y);
    }
    color(fg, 0.35);
    cr.set_line_width(1.0);
    let _ = cr.stroke();
    for (x, y) in contacts.into_iter().flatten() {
        let (x, y) = touch_position(x, y);
        cr.arc(x, y, 6.0 / scale, 0.0, std::f64::consts::TAU);
        color(accent, 1.0);
        let _ = cr.fill_preserve();
        color(fg, 1.0);
        cr.set_line_width(1.5 / scale);
        let _ = cr.stroke();
    }
    let _ = cr.restore();
    if let Some(svg) = svg {
        controller_art::draw(cr, svg, fg, 0.60);
    }
    let _ = cr.restore();
}

fn show_actions(ctx: &Context, cell: Cell, actions: &gtk::Box) {
    let Ok(doc) = ctx.document() else { return };
    while let Some(child) = actions.first_child() {
        actions.remove(&child);
    }
    let group = adw::PreferencesGroup::builder()
        .title(format!("Row {}, column {}", cell.row, cell.column))
        .description("Virtual gamepad actions")
        .margin_top(18)
        .build();
    actions.append(&group);
    let index = match region_index(&doc, cell) {
        Ok(index) => index,
        Err(error) => {
            group.add(&adw::ActionRow::builder().title(error).build());
            return;
        }
    };
    let region = index.and_then(|index| regions(&doc).ok()?.nth(index));
    for (field, label) in [("touch", "Touch"), ("click", "Click")] {
        let current = region
            .and_then(|table| table.get(field))
            .and_then(Item::as_str);
        let ctx = ctx.clone();
        group.add(&controller::dropdown(
            label,
            current,
            "No action",
            controller::GAMEPAD_TARGETS
                .into_iter()
                .map(|(key, label)| (key.into(), label.into())),
            move |target| {
                ctx.edit(|doc| set_output(doc, cell, field, target));
            },
        ));
    }

    for (kind, title) in [
        ("touch", "Keyboard and mouse · Touch"),
        ("click", "Keyboard and mouse · Click"),
    ] {
        let group = adw::PreferencesGroup::builder()
            .title(title)
            .margin_top(18)
            .build();
        let input = cell.input(kind);
        let edit_input = input.clone();
        let ctx = ctx.clone();
        for row in controller::keyboard_rows(&doc, &input, move |gesture, output| {
            ctx.edit(|doc| set_keyboard_action(doc, &edit_input, gesture, output));
        }) {
            group.add(&row);
        }
        actions.append(&group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Cell = Cell { row: 1, column: 2 };

    #[test]
    fn preview_parses_zero_one_or_two_contacts_in_surface_coordinates() {
        assert_eq!(parse_contacts(""), Some([None, None]));
        assert_eq!(parse_contacts("960,540"), Some([Some((0.5, 0.5)), None]));
        assert_eq!(
            parse_contacts("0,0;1919,1079"),
            Some([Some((0.0, 0.0)), Some((1919.0 / 1920.0, 1079.0 / 1080.0))])
        );
        for invalid in ["1920,0", "0,1080", "-1,0", "10", "0,0;", "0,0;1,1;2,2"] {
            assert!(parse_contacts(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn assigns_separate_touch_and_click_targets_and_removes_empty_regions() {
        let mut doc = "version = 1\n[gyro]\nsensitivity_x = 1.5\n"
            .parse()
            .unwrap();
        set_output(&mut doc, RIGHT, "touch", Some("cross")).unwrap();
        set_output(&mut doc, RIGHT, "click", Some("square")).unwrap();
        let saved = doc.to_string();
        let mut doc = saved.parse().unwrap();
        assert_eq!(grid(&doc), Ok(DEFAULT_SIZE));
        assert!(saved.contains("sensitivity_x = 1.5"));
        let region = regions(&doc).unwrap().next().unwrap();
        assert_eq!(Cell::read(region), Ok(RIGHT));
        assert_eq!(region["touch"].as_str(), Some("cross"));
        assert_eq!(region["click"].as_str(), Some("square"));

        set_output(&mut doc, RIGHT, "touch", None).unwrap();
        let region = regions(&doc).unwrap().next().unwrap();
        assert!(!region.contains_key("touch"));
        assert_eq!(region["click"].as_str(), Some("square"));
        set_output(&mut doc, RIGHT, "click", None).unwrap();
        let doc = doc.to_string().parse().unwrap();
        assert_eq!(regions(&doc).unwrap().count(), 0);
        assert_eq!(grid(&doc), Ok(DEFAULT_SIZE));
    }

    #[test]
    fn keyboard_actions_share_the_grid_and_preserve_other_gestures() {
        let input = RIGHT.input("click");
        let mut doc = "version = 1\n".parse::<DocumentMut>().unwrap();
        set_keyboard_action(&mut doc, &input, Gesture::Hold, Some("BTN_LEFT")).unwrap();
        assert!(doc.contains_key("touchpad"));
        let saved = doc.to_string().replace("click:1:2", "click:01:02");
        let mut doc = saved.parse::<DocumentMut>().unwrap();
        set_keyboard_action(&mut doc, &input, Gesture::Tap, Some("KEY_SPACE")).unwrap();
        let binding = bindings::keyboard_binding(&doc, &input).unwrap().unwrap();
        assert_eq!(binding["hold"].as_str(), Some("BTN_LEFT"));
        assert_eq!(binding["tap"].as_str(), Some("KEY_SPACE"));
        set_keyboard_action(&mut doc, &input, Gesture::Tap, None).unwrap();
        assert!(bindings::keyboard_binding(&doc, &input).unwrap().is_some());
    }

    #[test]
    fn shrinking_rejects_existing_references_without_editing_the_document() {
        let base = "version = 1\n[touchpad]\nrows = 1\ncolumns = 2\n";
        for bindings in [
            "[[touchpad.regions]]\nrow = 1\ncolumn = 2\nclick = 'square'\n",
            "[gyro]\nbinding = { mode = 'chord', input = 'l1,touch:1:2' }\n",
            "[[keyboard.bindings]]\nbutton = 'click:1:2'\ntap = 'KEY_E'\n",
            "[keyboard]\nbindings = [{ chord = ['l1', 'touch:1:2'], hold = 'KEY_E' }]\n",
        ] {
            let mut doc = format!("{base}{bindings}").parse::<DocumentMut>().unwrap();
            let before = doc.to_string();
            assert!(resize(&mut doc, FIRST_CELL).is_err(), "{bindings}");
            assert_eq!(doc.to_string(), before);
        }
    }

    #[test]
    fn resizing_preserves_remaining_cells_and_comments() {
        let mut doc = r#"
version = 1
[touchpad]
rows = 1
columns = 2 # region widths
[[touchpad.regions]]
row = 1
column = 1
touch = "circle" # keep this comment
"#
        .parse::<DocumentMut>()
        .unwrap();
        assert!(resize(&mut doc, Cell { row: 6, column: 5 }).is_err());
        resize(&mut doc, FIRST_CELL).unwrap();
        set_output(&mut doc, FIRST_CELL, "touch", Some("cross")).unwrap();
        let text = doc.to_string();
        assert!(text.contains("columns = 1 # region widths"));
        assert!(text.contains("touch = \"cross\" # keep this comment"));
    }
}
