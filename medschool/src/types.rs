use std::{fmt::Display, hash::Hash};

use syn::{Ident, Type, TypePath};

use crate::parse::docs_from_attributes;

/// We're extracting [`syn::Item`] structs and enums into this type.
///
/// Implements [`Ord`] by checking if any of its `PartialType::fields` belongs to another type that
/// we have declared.
#[derive(Debug, Default, Clone)]
pub struct PartialType {
    /// Only interested in the type name, e.g. `struct {Foo}`.
    pub ident: String,

    /// The docs of the item, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    pub docs: Vec<String>,

    /// Only useful when [`syn::ItemStruct`], we don't look into variants of enums.
    pub fields: Vec<PartialField>,
}

/// The fields when the item we're looking is [`syn::ItemStruct`].
///
/// Implements [`Ord`] by `PartialField::ty.len()`, thus making primitive types < custom types
/// (usually).
#[derive(Debug, Clone)]
pub struct PartialField {
    /// The name of the field, e.g. `{foo}: String`.
    pub ident: String,

    /// The type of the field, e.g. `foo: {String}`.
    ///
    /// When we encounter generics, such as `foo: Option<String>`, we dig into the generics to get
    /// the last [`syn::PathSegment`].
    ///
    /// Keep in mind that the handling for this is very crude, so if you have a field with anything
    /// fancier than just the `foo` example (such as `foo: bar::String`), the assumptions break.
    pub ty: String,

    /// The docs of the field, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    ///
    /// These docs will be merged with the type docs of this field's type, if it is a sub-type of
    /// any type that we declared.
    pub docs: Vec<String>,
}

impl PartialField {
    /// Converts a [`syn::Field`] into [`PartialField`], using
    /// [`get_ident_from_field_skipping_generics`] to get the field type.
    #[tracing::instrument(level = "trace", ret)]
    pub fn new(field: syn::Field) -> Option<Self> {
        let type_ident = match field.ty {
            Type::Path(type_path) => {
                // `get_ident` returns `Some` if the `path` doesn't contain generics.
                type_path
                    .path
                    .get_ident()
                    .cloned()
                    .or_else(|| Self::get_ident_from_field_skipping_generics(type_path))
            }
            _ => None,
        }?;

        Some(Self {
            ident: field.ident?.to_string(),
            ty: type_ident.to_string(),
            docs: docs_from_attributes(field.attrs),
        })
    }

    /// Digs into the generics of a field ([`syn::ItemStruct`] only), trying to get the last
    /// [`syn::PathArguments`], which (hopefully) contains the concrete type we care about.
    ///
    /// Extracts `Foo` from `foo: Option<Vec<{Foo}>>`.
    ///
    /// Doesn't handle generics of generics though, so if your field is `baz: Option<T>` we're going
    /// to be assigning this field type to be the string `"T"` (which is probably not what you
    /// wanted).
    #[tracing::instrument(level = "trace", ret)]
    fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
        // welp, this path probably contains generics
        let segment = type_path.path.segments.last()?;

        let mut current_argument = segment.arguments.clone();
        let mut inner_type = None;

        // keep digging into the `PathArguments`
        while let syn::PathArguments::AngleBracketed(generics) = &current_argument {
            // go directly to the last piece of generics, skipping lifetimes
            match generics.args.last()? {
                // finally have something that resembles a type, but might be an `Option`, so
                // we have to go deeper!
                syn::GenericArgument::Type(Type::Path(generic_path)) => {
                    // that's it, we've reached the final type
                    inner_type = match generic_path.path.segments.last() {
                        Some(t) => Some(t.ident.clone()),
                        None => break,
                    };

                    current_argument = generic_path.path.segments.last()?.arguments.clone();
                }
                _ => break,
            }
        }

        inner_type
    }
}

impl PartialOrd for PartialType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialOrd for PartialField {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PartialType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if other.fields.iter().any(|field| field.ty == self.ident) {
            // `other` has a field that is of type `self`, so it "belongs" to `self`
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    }
}

impl Ord for PartialField {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // primitive types < custom types (usually)
        self.ty
            .len()
            .cmp(&other.ty.len())
            // both field types have the same length, so sort alphabetically
            .then_with(|| self.ident.cmp(&other.ident))
    }
}

impl From<PartialField> for PartialType {
    fn from(value: PartialField) -> Self {
        Self {
            ident: value.ident,
            docs: value.docs,
            fields: Default::default(),
        }
    }
}

impl PartialEq for PartialType {
    fn eq(&self, other: &Self) -> bool {
        self.ident.eq(&other.ident)
    }
}

impl PartialEq for PartialField {
    fn eq(&self, other: &Self) -> bool {
        self.ty.eq(&other.ty)
    }
}

impl Eq for PartialType {}

impl Eq for PartialField {}

impl Hash for PartialType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Hash for PartialField {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Display for PartialType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the type is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)?;

        self.fields.iter().fold(f, |accum, s| {
            accum.write_str(&s.to_string()).expect("writing failed");
            accum
        });
        Ok(())
    }
}

impl Display for PartialField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the field is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)
    }
}
