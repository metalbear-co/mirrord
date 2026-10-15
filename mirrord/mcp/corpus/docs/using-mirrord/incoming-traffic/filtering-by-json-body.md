---
title: Traffic Filtering by JSON body
date: 2020-11-16T12:59:39.000Z
lastmod: 2025-02-24T00:00:00.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
weight: 100
toc: true
tags:
  - oss
  - team
  - enterprise
description: How to filter traffic by JSON body using mirrord
---

mirrord can steal incoming HTTP requests based on values inside a JSON request body. This allows matching on deeply nested fields and applying a regular expression to the extracted values.
This filter is available in the following mirrord.json configuration:

{% hint style="info" %}
This feature is available in mirrord version 3.175.0 and onwards.
{% endhint %}

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "body_filter": {
            "body": "json",
            "query": "$.data[*].legacyEntityIds.legacyUserIdentityId",
            "matches": "^identity123$",
          }
        }
      }
    }
  }
}
```

`body`: must be "json" for JSON body filtering.

`query`: JSONPath [(RFC 9535)](https://www.rfc-editor.org/rfc/rfc9535.html#name-jsonpath-examples) used to extract values from the parsed JSON.

`matches`: regex applied to each extracted value (after converting to string).

When using `any_of` or `all_of`, body filter fields are written directly in each condition, rather than under `body_filter`, example can be found below.

### Type Handling and the `typeof` Extension

mirrord stringifies all JSONPath query results before applying the regex.
To filter values by JSON type, mirrord provides a custom `typeof` function extension to `RFC 9535`

`typeof` returns one of:
```
"null" | "bool" | "number" | "string" | "array" | "object"
```

This allows writing queries like:
```json
"query": "$.items[?(typeof(@.price) == 'number')].price"
```

If the queried nodes do not share a single type, typeof returns 'nothing'.


## Overview
When enabled, mirrord:
1. **Read and parse the request body**

    mirrord reads the full request body into memory and attempts to parse it as JSON.
2. **Extract values with the JSONPath query**

    mirrord applies the user’s JSONPath expression in `query` field to the parsed JSON.
    - The query may return zero, one, or multiple values.
    - All extracted values are converted to strings before matching.
3. **Apply the regex from `matches` field**

    mirrord tests each extracted value against the regex.
    *The filter matches if at least one value matches.*
4. **Final decision**

    If the JSONPath extraction and regex match conditions succeed, the filter matches and mirrord may steal the request based on the overall filtering rules.
    If any step fails, the filter simply does not match.

### Processing Limits
mirrord applies two safeguards when reading request bodies for JSON filtering:

1. **Maximum body size**

    mirrord reads up to a configurable limit (default 65535 bytes, or 64 kb).
    The value is configured in bytes.
    Configure with `agent.max_body_buffer_size`.
    If the body exceeds this size, it is not fully read and the filter does not match.
2. **Read timeout**

    mirrord waits up to a configurable timeout (default 1000 ms, or 1 second) to read the full body.
    The value is configured in milliseconds.
    Configure with `agent.max_body_buffer_timeout`.
    If the body is not fully received in time, the filter does not match.

Both settings follow the same configuration mechanism as other agent parameters and can be set through the operator or in the [mirrord.json configuration](https://metalbear.com/mirrord/docs/config#:~:text=%22-,agent,-%22%3A%20%7B) file.
These limits ensure predictable performance and prevent excessive memory usage.

## Configuration Example
Configuration below applies to only steal requests with path `/orders` and have a JSON body with at least one numeric `price` value ending in "99".

```json
{
  "feature": {
    "network": {
      "incoming": {
        "http_filter": {
          "all_of": [
            {
              "path": "/orders"
            },
            {
              "body": "json",
              "query": "$..[?(typeof(@.price) == 'number')].price",
              "matches": "99$"
            }
          ]
        }
      }
    }
  }
}
```


## Examples and Results

### Request A matches and stolen:
```bash
POST /orders
Content-Type: application/json
```
```json
{
  "orderId": "123",
  "items": [
    { "name": "A", "price": 199 },
    { "name": "B", "price": 42 }
  ]
}
```


### Request B does not match and not stolen:
```bash
POST /orders
Content-Type: application/json
```
```json
{
  "orderId": "124",
  "items": [
    { "name": "A", "price": 150 },
    { "name": "B", "price": 42 }
  ]
}
```


### Request C does not match and not stolen:
```bash
POST /payments
Content-Type: application/json
```
```json
{
  "orderId": "125",
  "items": [
    { "name": "A", "price": 199 }
  ]
}
```
