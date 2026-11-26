use super::Key;

/// A bit per output ID up to the highest configured key, allocated at setup.
pub(super) struct KeySet(Box<[u64]>);

impl KeySet {
    pub(super) fn new(words: usize) -> Self {
        Self(vec![0; words].into_boxed_slice())
    }

    pub(super) fn clear(&mut self) {
        self.0.fill(0);
    }

    #[inline]
    pub(super) fn insert(&mut self, id: u16) {
        self.0[usize::from(id) / 64] |= 1 << (id % 64);
    }

    #[inline]
    pub(super) fn contains(&self, id: u16) -> bool {
        self.0
            .get(usize::from(id) / 64)
            .is_some_and(|word| word & (1 << (id % 64)) != 0)
    }

    #[inline]
    pub(super) fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.0
            .iter()
            .copied()
            .enumerate()
            .flat_map(|(index, mut word)| {
                std::iter::from_fn(move || {
                    if word == 0 {
                        return None;
                    }
                    let bit = word.trailing_zeros();
                    word &= word - 1;
                    Some(index as u16 * 64 + bit as u16)
                })
            })
    }
}

pub(super) fn key_set(words: usize, keys: impl Iterator<Item = Key>) -> KeySet {
    let mut set = KeySet::new(words);
    for key in keys {
        set.insert(key.id);
    }
    set
}
