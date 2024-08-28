Change permissions to use new `SubjectAccessReview` api instead of `impersonate`.

Added:
```yaml
- apiGroups:
  - authorization.k8s.io
  resources:
  - subjectaccessreviews
  verbs:
  - create
```

Removed:
```yaml
- apiGroups:
  - ""
  - authentication.k8s.io
  resources:
  - groups
  - users
  - userextras/accesskeyid
  - userextras/arn
  - userextras/canonicalarn
  - userextras/sessionname
  - userextras/iam.gke.io/user-assertion
  - userextras/user-assertion.cloud.google.com
  - userextras/principalid
  - userextras/oid
  - userextras/username
  - userextras/licensekey
  verbs:
  - impersonate
```
