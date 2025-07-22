//! The interface for the types we're extracting from the source code.

use std::{
    borrow::{Borrow, Cow},
    cmp::Ordering,
    collections::BTreeSet,
    fmt::Display,
    hash::Hash,
};

use syn::Type;

use crate::parse::{docs_from_attributes, get_ident_from_field_skipping_generics};

/// We're extracting [`syn::Item`] structs and enums into this type.
///
/// Implements [`Ord`] by checking if any of its `PartialType::fields` belongs to another type that
/// we have declared.
#[derive(Debug, Default, Clone)]
pub struct PartialType<'a> {
    /// Only interested in the type name, e.g. `struct {Foo}`.
    pub ident: Cow<'a, str>,

    /// The docs of the item, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    pub docs: Vec<String>,

    /// Only useful when [`syn::ItemStruct`], we don't look into variants of enums.
    pub fields: BTreeSet<PartialField<'a>>,
}

/// The fields when the item we're looking is [`syn::ItemStruct`].
///
/// Implements [`Ord`] by `PartialField::ty.len()`, thus making primitive types < custom types
/// (usually).
#[derive(Debug, Clone)]
pub struct PartialField<'a> {
    /// The name of the field, e.g. `{foo}: String`.
    pub ident: Cow<'a, str>,

    /// The type of the field, e.g. `foo: {String}`.
    ///
    /// When we encounter generics, such as `foo: Option<String>`, we dig into the generics to get
    /// the last [`syn::PathSegment`].
    ///
    /// Keep in mind that the handling for this is very crude, so if you have a field with anything
    /// fancier than just the `foo` example (such as `foo: bar::String`), the assumptions break.
    pub ty: Cow<'a, str>,

    /// The docs of the field, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    ///
    /// These docs will be merged with the type docs of this field's type, if it is a sub-type of
    /// any type that we declared.
    pub docs: Vec<String>,
}

/// Converts a [`syn::Field`] into [`PartialField`], using
/// [`get_ident_from_field_skipping_generics`] to get the field type.
impl TryFrom<syn::Field> for PartialField<'_> {
    type Error = ();

    #[tracing::instrument(level = "trace", ret)]
    fn try_from(field: syn::Field) -> Result<Self, Self::Error> {
        let type_ident = match field.ty {
            Type::Path(type_path) => {
                // `get_ident` returns `Some` if the `path` doesn't contain generics.
                type_path
                    .path
                    .get_ident()
                    .cloned()
                    .or_else(|| get_ident_from_field_skipping_generics(type_path))
            }
            _ => None,
        }
        .ok_or(())?;

        let ident = field.ident.as_ref().ok_or(())?.to_string();
        Ok(Self {
            ident: ident.into(),
            ty: type_ident.to_string().into(),
            docs: docs_from_attributes(field.attrs).ok_or(())?,
        })
    }
}

impl PartialType<'_> {
    /// Turns the `root` [`PartialType`] documentation into one big `String`.
    pub fn produce_docs(self) -> String {
        self.docs
            .into_iter()
            .chain(
                self.fields
                    .into_iter()
                    .flat_map(|field| field.docs.into_iter()),
            )
            .collect()
    }
}

impl PartialEq for PartialField<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.ident == other.ident
    }
}

impl Eq for PartialField<'_> {}

impl PartialOrd for PartialField<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PartialField<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ident.cmp(&other.ident)
    }
}

impl<'a> From<PartialField<'a>> for PartialType<'a> {
    fn from(value: PartialField<'a>) -> Self {
        Self {
            ident: value.ident,
            docs: value.docs,
            fields: Default::default(),
        }
    }
}

impl PartialEq for PartialType<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.ident.eq(&other.ident)
    }
}

impl Eq for PartialType<'_> {}

impl Hash for PartialType<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl<'a> Borrow<Cow<'a, str>> for PartialType<'a> {
    fn borrow(&self) -> &Cow<'a, str> {
        &self.ident
    }
}

impl Display for PartialType<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the type is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        formatter.write_str(&docs)?;

        self.fields.iter().try_for_each(|field| {
            formatter.write_str(&field.ident)?;
            Ok(())
        })?;
        Ok(())
    }
}

impl Display for PartialField<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the field is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        formatter.write_str(&docs)
    }
}
