use std::collections::BTreeMap;

use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
};
use kube::Resource;

pub trait SubjectAccessReviewSource {
    fn subject_user(&self) -> Option<String>;

    fn subject_groups(&self) -> Option<Vec<String>>;

    fn subject_extra(&self) -> Option<BTreeMap<String, Vec<String>>>;

    fn create_subject_access_review(
        &self,
        resource_attributes: ResourceAttributes,
    ) -> SubjectAccessReview {
        let spec = SubjectAccessReviewSpec {
            extra: self.subject_extra(),
            groups: self.subject_groups(),
            user: self.subject_user(),
            resource_attributes: Some(resource_attributes),
            ..Default::default()
        };

        SubjectAccessReview {
            spec,
            ..Default::default()
        }
    }

    fn create_resource_subject_access_review<R: Resource<DynamicType = ()>>(
        &self,
        namespace: Option<&str>,
        name: Option<&str>,
        verb: &str,
        subresource: Option<&str>,
    ) -> SubjectAccessReview {
        self.create_subject_access_review(ResourceAttributes {
            group: Some(R::group(&()).into_owned()),
            name: name.map(str::to_owned),
            namespace: namespace.map(str::to_owned),
            resource: Some(R::plural(&()).into_owned()),
            subresource: subresource.map(str::to_owned),
            verb: Some(verb.to_owned()),
            version: Some(R::version(&()).into_owned()),
        })
    }
}
