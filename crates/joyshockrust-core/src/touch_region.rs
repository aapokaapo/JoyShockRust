//! Touchpad grid geometry and input identifiers.

use core::str::FromStr;

pub const MAX_CELLS: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    rows: usize,
    columns: usize,
}

impl GridSize {
    pub fn new(rows: usize, columns: usize) -> Option<Self> {
        rows.checked_mul(columns)
            .filter(|count| (1..=MAX_CELLS).contains(count))?;
        Some(Self { rows, columns })
    }

    pub fn count(self) -> usize {
        self.rows * self.columns
    }

    pub fn cell_index(self, row: usize, column: usize) -> Option<usize> {
        if (1..=self.rows).contains(&row) && (1..=self.columns).contains(&column) {
            Some((row - 1) * self.columns + column - 1)
        } else {
            None
        }
    }

    /// Index a point already validated against the input surface dimensions.
    #[inline]
    pub fn contact_index(self, x: usize, y: usize, width: usize, height: usize) -> usize {
        let column = x * self.columns / width;
        let row = y * self.rows / height;
        row * self.columns + column
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Touch,
    Click,
}

impl TouchAction {
    pub fn split(input: &str) -> Option<(Self, &str)> {
        let (kind, location) = input.split_once(':')?;
        let action = match kind {
            "touch" => Self::Touch,
            "click" => Self::Click,
            _ => return None,
        };
        Some((action, location))
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Touch => "touch",
            Self::Click => "click",
        }
    }
}

pub fn parse_cell<T: FromStr>(location: &str) -> Result<(T, T), &'static str> {
    let (row, column) = location
        .split_once(':')
        .ok_or("expected ROW:COLUMN for a touchpad region")?;
    let row = row.parse().map_err(|_| "invalid touchpad region row")?;
    let column = column
        .parse()
        .map_err(|_| "invalid touchpad region column")?;
    Ok((row, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_indexes_match_contact_boundaries() {
        let size = GridSize::new(1, 2).unwrap();
        assert_eq!(size.cell_index(1, 2), Some(1));
        assert_eq!(size.cell_index(0, 1), None);
        assert_eq!(size.contact_index(959, 0, 1920, 1080), 0);
        assert_eq!(size.contact_index(960, 0, 1920, 1080), 1);
        assert!(GridSize::new(5, 5).is_some());
        assert!(GridSize::new(2, 13).is_none());
        assert!(GridSize::new(usize::MAX, 2).is_none());
    }
}
