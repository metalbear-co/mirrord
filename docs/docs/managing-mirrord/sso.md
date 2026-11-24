---
title: Single Sign-On
description: Connect the mirrord Operator to your OIDC identity provider so developers log in without a kubeconfig.
tags:
  - team
  - enterprise
---

# Single Sign-On

With single sign-on (SSO), developers log in to the mirrord Operator with your company's OpenID Connect (OIDC) identity provider, such as Okta, Microsoft Entra ID, Google Workspace or Keycloak, instead of through a kubeconfig. The Operator checks the provider's ID token itself and maps it to a Kubernetes username and groups, following the same rules as the API server's `--oidc-*` flags. Existing RoleBindings keep matching, because an SSO user has the same name an OIDC kubeconfig for the same provider would give them.

This is useful when developers have no cluster access of their own, when your clusters are reachable only through a bastion or VPN that developer machines are not on, or when you want one login for mirrord across clusters.

SSO setup needs Operator `3.232.0` or later. Older versions ignore the `sso` Helm values. This page covers the setup an admin does once: registering mirrord at the identity provider, turning SSO on in the Helm chart, and granting groups access. The Operator then accepts ID tokens from your provider at the SSO URL. The CLI and IDE login that obtains those tokens for developers ships in a later release.

## 1. Register mirrord at your identity provider

Create an OIDC application (sometimes called a client, an app registration or an app integration) for mirrord. You need three things from it:

- The **issuer URL**, as the provider announces it. It must serve the discovery document at `<issuer URL>/.well-known/openid-configuration`.
- The **client ID**.
- The **client secret**. Register mirrord as a confidential (web) application, so the provider issues one.

Make sure the ID token carries the claims you want to use for usernames and groups:

- **Username**: the default claim is `sub`, which is stable but opaque. Most teams use `email` instead. When you do, the provider must mark it verified: a token whose `email_verified` claim is `false` is refused, as the API server refuses it.
- **Groups**: the provider adds a groups claim only when you ask for it. In Okta, add a `groups` claim to the ID token in the app's **Sign On** settings. In Entra ID, enable **Groups claim** in **Token configuration**. In Keycloak, add a **Group Membership** mapper to the client. The claim value may be a string or a list of strings.

## 2. Store the client secret

The client secret never goes into the values file. Put it in a Secret in the Operator's namespace, and point the chart at it. Read the secret into a variable first, so characters like `$` or `&` in it reach the Secret unchanged:

```bash
read -rs CLIENT_SECRET
kubectl create secret generic mirrord-sso -n mirrord --from-literal=clientSecret="$CLIENT_SECRET"
```

## 3. Turn SSO on in the Helm chart

```yaml
sso:
  enabled: true
  url: https://mirrord.example.com
  issuerUrl: https://login.example.com/oauth2/default
  clientId: 0oa1b2c3d4e5f6g7h8i9
  clientSecretRef:
    name: mirrord-sso
    key: clientSecret
  usernameClaim: email
  groupsClaim: groups
  groupsPrefix: "oidc:"
  ingress:
    className: nginx
    tlsSecret: mirrord-example-com-tls
```

| Value | API server flag | What it does |
| --- | --- | --- |
| `url` | | The HTTPS URL developers reach the Operator at. The chart creates an Ingress for its host. Required. |
| `issuerUrl` | `--oidc-issuer-url` | The provider's issuer URL. The Operator refuses tokens from any other issuer. Required. |
| `clientId` | `--oidc-client-id` | The client ID of the mirrord application. The Operator refuses tokens issued for any other audience. Required. |
| `clientSecretRef` | | Name and key of the Secret holding the client secret. Required. |
| `usernameClaim` | `--oidc-username-claim` | Claim that becomes the Kubernetes username. Default `sub`. |
| `usernamePrefix` | `--oidc-username-prefix` | Prefix put in front of the username. See [Usernames and groups](#usernames-and-groups). |
| `groupsClaim` | `--oidc-groups-claim` | Claim that holds the user's groups. Without it, SSO users have no groups beyond `system:authenticated`. |
| `groupsPrefix` | `--oidc-groups-prefix` | Prefix put in front of every group. |
| `ingress` | | The Ingress for the host in `url`. `tlsSecret` is required while it is enabled. Set `enabled: false` to route the host yourself. |

`helm install` and `helm upgrade` fail when `sso.enabled` is `true` and a required value is empty, naming the value to set.

### Exposing the URL

The chart's Ingress sends the `/apis/operator.metalbear.co/` path of the host in `sso.url` to the Operator Service on the Operator's own HTTPS port, the one the API server also uses. Only that path is forwarded: the Operator's admission webhook and health endpoint stay inside the cluster. The Operator keeps terminating TLS with the certificate in `tls.secret`, so the Ingress controller has to speak HTTPS to the backend. The chart sets the ingress-nginx annotation for that, `nginx.ingress.kubernetes.io/backend-protocol: "HTTPS"`, by default in `sso.ingress.annotations`; replace it with your controller's equivalent if you use another one. `tlsSecret` names the certificate the Ingress serves the host with and is required while the Ingress is enabled, so the login never travels over plain HTTP. Point the host's DNS record at the Ingress.

If you use the Gateway API, a cloud load balancer or a service mesh instead, set `sso.ingress.enabled: false` and route the host to the `mirrord-operator` Service on port `443` (the `operator.port` value) over HTTPS yourself. Forward the `/apis/operator.metalbear.co/` path only.

The Operator tells the two paths apart by the client certificate the API server presents, not by the route. A request arriving through the SSO URL is accepted only with a valid ID token from your provider; any `X-Remote-User` or `X-Remote-Group` header it carries is dropped. The Operator then asks the cluster's RBAC, with a `SubjectAccessReview`, whether that user may perform the request, the same check the API server does before it forwards a kubectl call to the Operator. A user without a matching RoleBinding gets a `403`.

## Usernames and groups

The Operator builds the Kubernetes username and groups exactly as the API server does for an OIDC kubeconfig with the same settings:

| Setting | Resulting name |
| --- | --- |
| `usernameClaim: sub`, no `usernamePrefix` | `<issuerUrl>#<sub>`, for example `https://login.example.com/oauth2/default#00u1a2b3c4` |
| `usernameClaim: email`, no `usernamePrefix` | the email as is, for example `jim@example.com` |
| any claim with `usernamePrefix: "oidc:"` | `oidc:<claim value>` |
| any claim with `usernamePrefix: "-"` | the claim value as is |
| `groupsClaim: groups`, no `groupsPrefix` | each group as is, for example `platform-team` |
| `groupsClaim: groups`, `groupsPrefix: "oidc:"` | `oidc:platform-team` |

Every SSO user is also in the `system:authenticated` group, as every logged-in Kubernetes user is.

If your clusters already authenticate kubectl users against the same provider, copy the API server's `--oidc-*` flag values into the matching `sso` values and the RoleBindings you have match SSO users without changes.

Names that start with `system:` are reserved by Kubernetes and refused. If a claim can produce one, set `usernamePrefix` or `groupsPrefix`.

## Granting a group access to a namespace

RBAC for SSO users is the same as for any Kubernetes user. See [How do I configure Role Based Access Control](security.md#how-do-i-configure-role-based-access-control-for-mirrord-for-teams) for the roles the chart creates. To let everyone in the `platform-team` group run mirrord on targets in the `staging` namespace, with the `groupsPrefix: "oidc:"` from the example above:

1. Bind the group to the cluster-wide `mirrord-operator-user-basic` ClusterRole, which grants what every mirrord user needs regardless of namespace:

   ```yaml
   apiVersion: rbac.authorization.k8s.io/v1
   kind: ClusterRoleBinding
   metadata:
     name: mirrord-platform-team
   subjects:
   - kind: Group
     name: oidc:platform-team
     apiGroup: rbac.authorization.k8s.io
   roleRef:
     kind: ClusterRole
     name: mirrord-operator-user-basic
     apiGroup: rbac.authorization.k8s.io
   ```

2. Add `staging` to the chart's `roleNamespaces` value, so the chart creates the namespaced `mirrord-operator-user` Role there, and bind the group to it:

   ```yaml
   apiVersion: rbac.authorization.k8s.io/v1
   kind: RoleBinding
   metadata:
     name: mirrord-platform-team
     namespace: staging
   subjects:
   - kind: Group
     name: oidc:platform-team
     apiGroup: rbac.authorization.k8s.io
   roleRef:
     kind: Role
     name: mirrord-operator-user
     apiGroup: rbac.authorization.k8s.io
   ```

To grant a group every namespace instead, bind it to the `mirrord-operator-user` ClusterRole, as in the [RBAC examples](security.md#how-do-i-configure-role-based-access-control-for-mirrord-for-teams). Users still need `get` on the workloads they target, the same as without SSO.

## Checking the setup

Until the CLI login ships, you can check the setup with an ID token obtained from your provider in any other way, for example from the provider's token endpoint or a test application, as long as it was issued for mirrord's client ID:

```bash
curl -H "Authorization: Bearer $ID_TOKEN" https://mirrord.example.com/apis/operator.metalbear.co/v1/mirrordoperators/operator
```

A valid token for a user with the RBAC above returns the Operator status. A refused one returns a `401` with a message that starts with `SSO login refused:` and names the reason, for example an expired token, an issuer or audience that does not match the values, a missing username claim, or a name in the reserved `system:` space. A `403` names the user, verb and resource that RBAC denied; check the RoleBindings against the username and groups the token maps to. A `503` means the Operator could not reach the provider's discovery document or signing keys from inside the cluster; check egress from the Operator pod to the issuer URL. The Operator's log has the same reasons at `debug` level.
