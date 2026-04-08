use std::{
    fmt,
    marker::PhantomData,
    ops::{Index, IndexMut},
};

use strum::VariantArray;

/// Trait for a unit enum that can be used as a key in [`EnumMap`].
pub trait EnumKey: VariantArray {
    /// Returns the index of this value inside [`VariantArray::VARIANTS`].
    fn into_index(self) -> usize;
}

/// An array of values indexed by enum `E`.
///
/// **Important:** `SIZE` must be equal to the length of [`VariantArray::VARIANTS`] len.
pub struct EnumMap<E: EnumKey, T, const SIZE: usize> {
    values: [T; SIZE],
    _marker: PhantomData<fn() -> E>,
}

impl<E: EnumKey, T, const SIZE: usize> EnumMap<E, T, SIZE> {
    pub fn into_values(self) -> [T; SIZE] {
        self.values
    }

    pub fn iter(&self) -> impl Iterator<Item = (&E, &T)> {
        E::VARIANTS.iter().zip(&self.values)
    }
}

impl<E: EnumKey, T: Default, const SIZE: usize> Default for EnumMap<E, T, SIZE> {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| Default::default()),
            _marker: PhantomData,
        }
    }
}

impl<E: EnumKey, T, const SIZE: usize> Index<E> for EnumMap<E, T, SIZE> {
    type Output = T;

    #[allow(clippy::indexing_slicing)]
    fn index(&self, index: E) -> &Self::Output {
        &self.values[index.into_index()]
    }
}

impl<E: EnumKey, T, const SIZE: usize> IndexMut<E> for EnumMap<E, T, SIZE> {
    #[allow(clippy::indexing_slicing)]
    fn index_mut(&mut self, index: E) -> &mut Self::Output {
        &mut self.values[index.into_index()]
    }
}

impl<E: EnumKey + fmt::Debug, T: fmt::Debug, const SIZE: usize> fmt::Debug for EnumMap<E, T, SIZE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (variant, value) in E::VARIANTS.iter().zip(&self.values) {
            map.entry(variant, value);
        }
        map.finish()
    }
}
