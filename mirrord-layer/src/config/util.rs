use std::slice::Join;

use serde::Deserialize;

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(untagged)]
pub enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

impl<T> FlagField<T> {
    pub fn enabled_or_equal<'a, Rhs>(&'a self, comp: Rhs) -> bool
    where
        &'a T: PartialEq<Rhs>,
    {
        match self {
            FlagField::Enabled(enabled) => *enabled,
            FlagField::Config(val) => val == comp,
        }
    }

    pub fn map<'a, R, C>(&'a self, config_cb: C) -> R
    where
        R: From<bool>,
        C: FnOnce(&'a T) -> R,
    {
        match self {
            FlagField::Enabled(enabled) => R::from(*enabled),
            FlagField::Config(val) => config_cb(val),
        }
    }

    pub fn enabled_map<'a, R, E, C>(&'a self, enabled_cb: E, config_cb: C) -> R
    where
        E: FnOnce(bool) -> R,
        C: FnOnce(&'a T) -> R,
    {
        match self {
            FlagField::Enabled(enabled) => enabled_cb(*enabled),
            FlagField::Config(val) => config_cb(val),
        }
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(untagged)]
pub enum VecOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

impl<T> VecOrSingle<T> {
    pub fn join<Separator>(self, sep: Separator) -> <[T] as Join<Separator>>::Output
    where
        [T]: Join<Separator>,
    {
        match self {
            VecOrSingle::Single(val) => [val].join(sep),
            VecOrSingle::Multiple(vals) => vals.join(sep),
        }
    }
}
