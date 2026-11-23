use adw::{gtk, prelude::*};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value, value};

use crate::{
    bindings::replace_value,
    controller,
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

    fn from_input(input: &str) -> Option<Self> {
        let (kind, location) = input.trim().split_once(':')?;
        if !matches!(kind, "touch" | "click") {
            return None;
        }
        let (row, column) = location.split_once(':')?;
        Some(Self {
            row: row.parse().ok()?,
            column: column.parse().ok()?,
        })
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
    let apply = gtk::Button::with_label("Set Grid");
    dimensions.set_header_suffix(Some(&apply));
    dimensions.add(&rows);
    dimensions.add(&columns);
    page.add(&dimensions);

    let cells = adw::PreferencesGroup::new();
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    cells.add(&layout);
    page.add(&cells);
    cells.add(&actions);
    show_cells(ctx, size, &layout, &actions);

    let ctx = ctx.clone();
    let layout = layout.downgrade();
    let actions = actions.downgrade();
    let rows = rows.downgrade();
    let columns = columns.downgrade();
    apply.connect_clicked(move |_| {
        let (Some(layout), Some(actions), Some(rows), Some(columns)) = (
            layout.upgrade(),
            actions.upgrade(),
            rows.upgrade(),
            columns.upgrade(),
        ) else {
            return;
        };
        let size = Cell {
            row: rows.value() as i64,
            column: columns.value() as i64,
        };
        if ctx.edit(|doc| resize(doc, size)) {
            show_cells(&ctx, size, &layout, &actions);
        }
    });
    page.upcast()
}

fn dimension_row(title: &str, initial: i64) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(1.0, MAX_CELLS as f64, 1.0);
    row.set_title(title);
    row.set_snap_to_ticks(true);
    row.set_value(initial as f64);
    row
}

fn show_cells(ctx: &Context, size: Cell, layout: &gtk::Box, actions: &gtk::Box) {
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
                button.set_active(true);
                first = Some(button.clone());
            }
            let ctx = ctx.clone();
            let actions = actions.downgrade();
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(actions) = actions.upgrade()
                {
                    show_actions(&ctx, cell, &actions);
                }
            });
            grid.attach(&button, column as i32 - 1, row as i32 - 1, 1, 1);
        }
    }
    show_actions(ctx, FIRST_CELL, actions);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Cell = Cell { row: 1, column: 2 };

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
