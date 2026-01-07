use std::fmt::Debug;

use kube::{Api, Resource, api::ListParams, core::ErrorResponse};
use serde::de::DeserializeOwned;

use crate::error::Result;

const DEFAULT_LIST_LIMIT: u32 = 25;

/// Extensions trait for [`kube::Api`].
pub(super) trait ApiExt<K: Resource> {
    /// Searches for the first match of the `name` or `generated_name` fields against
    /// the provided glob pattern. Returns `Ok(None)` if nothing found.
    ///
    /// See pattern details [`here`](https://docs.rs/glob/latest/glob/struct.Pattern.html)
    async fn search_one_opt(&self, pattern: impl AsRef<str>) -> Result<Option<K>>;

    /// Similar to [`Self::search_one_opt`] but it returns error if no items have found.
    async fn search_one(&self, pattern: impl AsRef<str>) -> Result<K> {
        Ok(self
            .search_one_opt(pattern.as_ref())
            .await?
            .ok_or_else(|| {
                kube::Error::Api(ErrorResponse {
                    status: "Failure".to_owned(),
                    message: format!("resource not found for pattern '{}'", pattern.as_ref()),
                    reason: "NotFound".to_owned(),
                    code: 404,
                })
            })?)
    }
}

impl<K> ApiExt<K> for Api<K>
where
    K: Resource + Clone + DeserializeOwned + Debug,
{
    async fn search_one_opt(&self, pattern: impl AsRef<str>) -> Result<Option<K>> {
        let pattern = glob::Pattern::new(pattern.as_ref())?;
        let mut params = ListParams::default().limit(DEFAULT_LIST_LIMIT);
        loop {
            let list = self.list_metadata(&params).await?;
            for item in &list {
                let Some(name) = item
                    .meta()
                    .name
                    .as_ref()
                    .or(item.meta().generate_name.as_ref())
                else {
                    continue;
                };
                if pattern.matches(name) {
                    return Ok(Some(self.get(name).await?));
                }
            }
            match list.metadata.continue_ {
                Some(continue_token) => {
                    params.continue_token = Some(continue_token);
                }
                None => return Ok(None),
            };
        }
    }
}
