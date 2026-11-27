---
title: Cypress
date: 2026-11-27T00:00:00.000Z
lastmod: 2026-11-27T00:00:00.000Z
draft: false
toc: true
tags:
  - oss
  - team
  - enterprise
description: "Run your Cypress E2E tests against shared staging, and send only your test traffic to the service that you changed on your machine."
---

# Cypress

Use mirrord to test a change to one service with your team's Cypress suite, against a shared staging cluster. The changed service runs on your machine with `mirrord up`. Cypress adds a header to the requests of the browser. mirrord sends the requests with your header to your machine. All other requests go to the deployed service, so other people keep the usual staging behavior.

A developer can do this, and an AI agent can do the same steps after it changes a service.

![The test runner drives a browser that sends requests with the header baggage: mirrord-session=alice to the staging frontend. The frontend propagates the header to the greeting service. The mirrord agent matches the header and sends the request through the operator to the changed greeting service on your machine. Requests from other users have no header, and the deployed greeting service handles them.](../../.gitbook/assets/e2e-tests-header-routing.svg)

The examples on this page use a frontend that serves a page. The JavaScript of the page calls `/api/greeting` on the frontend, and the frontend calls the `greeting` service. You change `greeting` so that it returns `hello from my change`. The deployed `greeting` returns `hello from staging`.

## Prerequisites

* The mirrord CLI. See [CLI](../../installing-mirrord/cli.md).
* Cypress in your project. The examples use Cypress 16. See [Install Cypress](https://docs.cypress.io/app/get-started/install-cypress) on docs.cypress.io.
* Access to the staging cluster from your machine or CI runner.
* A frontend that propagates the header to the service that you change. See [Propagate the header](#propagate-the-header).

{% hint style="info" %}
To let several developers or CI jobs steal from the same service at the same time, [install the mirrord operator](../../getting-started/install-the-operator.md).
{% endhint %}

## Run the changed service with mirrord up

Put a `mirrord-up.yaml` file in the directory of the service:

```yaml
services:
  greeting:
    target:
      path: deployment/greeting
      namespace: staging
    http_filter:
      header_filter: '^baggage: .*mirrord-session={{ key }}(?:[,; ]|$)'
    run:
      command: ["node", "greeting.js"]
```

The service uses the default `split` mode. mirrord steals only the requests that match `header_filter`, and the deployed `greeting` handles all other requests. `{{ key }}` is the session key that you give with `--key`.

The filter ends at a delimiter: a comma, a semicolon, a space, or the end of the header. Thus the key `alice-e2e` does not match the header of the key `alice-e2e2`. The default filter of `mirrord up` has no delimiter, so set your own filter.

Start the service with a key that only your test run uses:

```sh
mirrord up --key alice-e2e
```

When the session is ready, `mirrord up` shows `Ready!`:

```
greeting: session started
greeting: Session key: alice-e2e
greeting: running process
greeting: Ready!
```

{% hint style="info" %}
When an AI agent or a script starts `mirrord up` with no terminal, set `MIRRORD_PROGRESS_MODE=simple` to get one line for each progress step. Add `--no-ui` if you do not need `mirrord ui`.
{% endhint %}

See [mirrord up](../multiple-concurrent-sessions.md) for all the options of `mirrord-up.yaml`.

## Add the header in Cypress

The header is `baggage: mirrord-session=<key>`. Give the key to Cypress in the `MIRRORD_KEY` environment variable.

### Give the key to the tests

Cypress 16 removed `Cypress.env()`. Put the key in the `expose` section of `cypress.config.js`, and read it with `Cypress.expose()` ([Migrating away from `Cypress.env()`](https://docs.cypress.io/app/references/migration-guide#Migrating-away-from-Cypressenv)):

```js
const { defineConfig } = require('cypress');

module.exports = defineConfig({
  expose: {
    MIRRORD_KEY: process.env.MIRRORD_KEY,
  },
  e2e: {
    baseUrl: process.env.BASE_URL,
    supportFile: false,
  },
});
```

On Cypress 15 and earlier, put the key in `env` and read it with `Cypress.env()`.

### Add the header with cy.intercept

Use [`cy.intercept()`](https://docs.cypress.io/api/commands/intercept) with a route handler, and set `req.headers['baggage']`. Match the staging host only, so that third-party hosts do not get the header. Set `middleware: true` so that the handler runs before other handlers of the same request:

```js
const stagingHost = new URL(Cypress.config('baseUrl')).hostname;

describe('greeting', () => {
  it('shows the greeting from my change', () => {
    cy.intercept({ hostname: stagingHost, middleware: true }, (req) => {
      req.headers['baggage'] = `mirrord-session=${Cypress.expose('MIRRORD_KEY')}`;
    });
    cy.visit('/');
    cy.get('#greeting').should('have.text', 'hello from my change');
  });

  it('shows the staging greeting without the header', () => {
    cy.visit('/');
    cy.get('#greeting').should('have.text', 'hello from staging');
  });
});
```

Cypress removes the intercepts after each test, so the second test sends no header. To add the header in all tests, put the `cy.intercept()` call in a `beforeEach()` in your support file.

### What gets the header

Only the requests that Cypress intercepts get the header. Cypress intercepts the requests of your front-end application: page loads, `fetch`, XHR, and other resources. Some requests do not get the header:

* `cy.request()` calls. Cypress does not intercept them. Add the header in the `headers` option of `cy.request()`.
* Requests to hosts that the route matcher does not match.
* The `headers` option of `cy.visit()` covers only the first request of the visit. Use `cy.intercept()` for the requests that the page makes after it loads.

The browser network tab does not show a header that `cy.intercept()` adds, because Cypress adds it after the request leaves the browser.

## Propagate the header

mirrord sees only the request that reaches `greeting`. The browser sends the header to the frontend, and the frontend makes a new request to `greeting`. The frontend must copy the header to that request. If it does not, the filter does not match, and the deployed `greeting` answers.

* If your services use OpenTelemetry, enable W3C `baggage` propagation. Then the header goes to the next service with no other change.
* If you use a header other than `baggage`, the frontend must copy that header name.

{% hint style="info" %}
**Let your AI agent set this up.** The [`mirrord-header-propagation`](https://github.com/metalbear-co/skills/tree/main/skills/mirrord-header-propagation) skill finds the HTTP, gRPC, and queue flows in your services and enables `baggage` propagation. [Install the mirrord skills](../../using-mirrord-with-ai/ai-skills-plugin.md), then ask your agent to *"propagate the mirrord session header across my services."*
{% endhint %}

For more information, see [Header Propagation](../../sharing-the-cluster/header-propagation.md).

## Run the suite on your machine

1. Start the changed service with `mirrord up --key alice-e2e`.
2. In a second terminal, set the key and the URL of the staging frontend:

   ```sh
   export MIRRORD_KEY=alice-e2e
   export BASE_URL=https://staging.example.com
   ```

3. Run the suite:

   ```sh
   npx cypress run
   ```

4. Stop `mirrord up` with `Ctrl+C`.

The output shows that the test with the header gets the local greeting, and the test without it gets the staging greeting:

```
  Running:  greeting.cy.js                                                  (1 of 1)

  greeting
    ✓ shows the greeting from my change (175ms)
    ✓ shows the staging greeting without the header (71ms)

  2 passing (260ms)
```

## Run the suite in CI

In CI, use `mirrord ci start`. It starts the service in the background and returns, so the next step can run the tests. When the operator is installed, it uses the CI API key, and the session does not take a seat. See [mirrord for CI](../../use-cases/mirrord-for-ci.md).

`mirrord ci start` takes the arguments of `mirrord exec` and reads a `mirrord.json` file. It does not read `mirrord-up.yaml`. Put the same target and filter in a `mirrord.json` file:

```json
{
  "target": {
    "path": "deployment/greeting",
    "namespace": "staging"
  },
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}(?:[,; ]|$)"
        }
      }
    }
  }
}
```

`mirrord ci start` returns when the local process starts, before the steal is active. A request that you send at once can still go to the deployed service. Wait until your change answers through staging, then run the tests.

This script works in most CI systems. It uses the run ID and the attempt number of GitHub Actions to make a unique key. Use the run ID of your CI system:

```bash
export MIRRORD_KEY="ci-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
export MIRRORD_PROGRESS_MODE=simple
export BASE_URL=https://staging.example.com

mirrord ci start -f mirrord.json --key "$MIRRORD_KEY" -- node greeting.js

# Wait until requests with the header reach the local service.
for i in $(seq 1 60); do
  curl -s -H "baggage: mirrord-session=$MIRRORD_KEY" "$BASE_URL/api/greeting" \
    | grep -q "my change" && break
  sleep 1
done

status=0
npx cypress run || status=$?
mirrord ci stop
exit $status
```

Replace the `grep` check with a check that only your change passes, for example a version field in the response. Set the `MIRRORD_CI_API_KEY` secret when the operator is installed. `mirrord ci start` shows this output, and then the tests run:

```
session started
Session key: ci-1795808949
running process
Ready!
mirrord ci files will be stored in .../mirrord/node-1795808950-oXG65na
child pid: 72642
```

## Troubleshooting

| Symptom | Cause and fix |
|---------|---------------|
| The tests with the header get the staging response. A request with the header sent directly to the service gets your response. | The frontend does not propagate the header. See [Propagate the header](#propagate-the-header). To see the headers that reach the service, use [`mirrord dump`](../incoming-traffic/inspect-live-traffic.md). |
| A test fails with `` `Cypress.env()` was removed in Cypress version 16.0.0 ``. | Put the key in `expose` and read it with `Cypress.expose()`. See [Give the key to the tests](#give-the-key-to-the-tests). |
| `cy.request()` calls get the staging response. | Cypress does not intercept `cy.request()`. Add the header in the `headers` option of `cy.request()`. |
| The first tests in CI get the staging response, and the later tests pass. | `mirrord ci start` returns before the steal is active. Wait until your change answers, then run the tests. |
| Your session gets the requests of another key, such as `alice-e2e2`. | The filter has no delimiter after the key. End the filter with a delimiter group, as in [the example](#run-the-changed-service-with-mirrord-up). |
| The progress output of `mirrord up` or `mirrord ci start` is unclear in a log file. | There is no terminal. Set `MIRRORD_PROGRESS_MODE=simple`. |
| A second developer cannot start a session on the same service. | Without the operator, only one session can steal from a target. [Install the mirrord operator](../../getting-started/install-the-operator.md). |

## FAQ

### Do other developers see my change?

No. mirrord sends only the requests with your key to your machine. The deployed service handles all other requests.

### Can several developers and CI jobs run the suite at the same time?

Yes, with the mirrord operator. Give each run a unique key, and use a filter that ends at a delimiter.

### Can I use a header other than `baggage`?

Yes. Change `header_filter` and the header name in `req.headers`. The frontend must then propagate that header name. With `baggage`, OpenTelemetry can propagate the header for you.

### Why does the browser network tab not show the header?

Cypress adds the header after the request leaves the browser. To see the header, wait on the intercept with `cy.wait()` and read `request.headers`, or use [`mirrord dump`](../incoming-traffic/inspect-live-traffic.md) on the service.

### Why do I use `mirrord up` on my machine and `mirrord ci start` in CI?

`mirrord up` runs in the foreground and stops when you stop it. `mirrord ci start` runs the service in the background, so the CI job can continue to the tests. It also uses the CI API key, so the session does not take a seat.

### Do I need the mirrord browser extension?

No. Cypress adds the header. To send the header from your own browser, see [Debugging from Browser](../incoming-traffic/debug-from-browser.md).

## See also

* [Playwright](playwright.md)
* [mirrord up](../multiple-concurrent-sessions.md)
* [Filtering Incoming Traffic](../incoming-traffic/filter-incoming-traffic.md)
* [Header Propagation](../../sharing-the-cluster/header-propagation.md)
* [mirrord for CI](../../use-cases/mirrord-for-ci.md)
* [Preview Environments in CI](../../use-cases/preview-environments-in-ci.md)
