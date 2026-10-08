---
title: Dashboard
description: Dashboard for monitoring mirrord usage
tags:
  - alpha
  - team
  - enterprise
---

# Dashboard

The mirrord Dashboard is a web-based interface for monitoring mirrord usage across your organization. It provides real-time visibility into sessions, users, targets, CI pipelines, and overall adoption trends.


{% hint style="success" %}
Want to see the dashboard in action? Check out our [live playground](https://playground.metalbear.dev/dashboard/).
{% endhint %}


mirrord serves this dashboard two ways, depending on how your operator is licensed:


| | [License Server Setup](dashboard/license-server.md) | [Cloud Setup](dashboard/cloud.md) |
| --- | --- | --- |
| **Operator credential** | Self-hosted license server (`license.licenseServer`) | Cloud API key (`cloud.apiKey`) |
| **Plan** | Enterprise | Team or Enterprise |
| **Where you view it** | In-cluster, via `kubectl port-forward` | [app.metalbear.com](https://app.metalbear.com), behind your login |
| **Access control** | Your cluster networking | Your organization's members, through login |
| **Where usage data lives** | Your license server's database | mirrord cloud |
| **Best for** | Self-hosted installs — air-gapped, compliance, or by preference | Teams that can reach the mirrord cloud |

An operator does one or the other, never both. Pick your setup path, then come back here — the interface below is identical either way.


| Dark mode                                                       | Light mode                                                        |
| --------------------------------------------------------------- | ----------------------------------------------------------------- |
| ![Dashboard - Dark Mode](../.gitbook/assets/dashboard-dark.png) | ![Dashboard - Light Mode](../.gitbook/assets/dashboard-light.png) |


## Usage Tab

The Usage tab is the main view, showing metrics, session activity, and analytics charts.

### Metric Cards and Session Activity

![Metric cards and session activity table](../.gitbook/assets/metrics-and-activity.png)

The top row displays five key metrics at a glance:

| Metric               | Description                                                   |
| -------------------- | ------------------------------------------------------------- |
| **Licenses**         | Total license count and number of active unique machines      |
| **mirrord Champion** | The most active mirrord user and their total session time     |
| **Total Sessions**   | Cumulative number of mirrord sessions, with a sparkline trend |
| **Session Time**     | Total cumulative session time, with a sparkline trend         |
| **CI Sessions**      | Total CI pipeline sessions, with max concurrency              |

Use the time range selector in the top-right corner (**7d**, **30d**, **90d**, **All**) to adjust the data to the selected time period.

Below the metrics, the **Session Activity** table shows a cross-referenced view of users and their target workloads. Each row shows the user, target, namespace, session count with a visual bar, and total time. The table is searchable and sortable by any column, with pagination for large datasets.

### Users View

![User charts and metrics table](../.gitbook/assets/users-view.png)

Switch to the **Users** tab to see user-focused analytics:

* **Session Duration vs. Count**: A scatter chart plotting each user's total sessions against their average session duration. Larger bubbles indicate more total session time, helping you spot power users and usage patterns.
* **User Timeline**: Shows when each user was first seen and their most recent activity, giving a quick view of adoption over time.
* **User Metrics** table: A detailed, searchable table with columns for identifier, first active date, last seen date, total sessions, cumulative time, and average duration. Click any column header to sort.

### Targets View

![Target adoption and namespace breakdown](../.gitbook/assets/targets-view.png)

Switch to the **Targets** tab to see target-focused analytics:

* **Target Adoption**: A scatter chart showing sessions vs. unique users per target. This helps identify which workloads are broadly adopted vs. heavily used by a few people.
* **Sessions by Namespace**: A horizontal bar chart breaking down session distribution across Kubernetes namespaces.
* **Target Metrics** table: A searchable table listing each target workload with its namespace, session count, total time, and number of unique users.

## ROI Calculator

![ROI Calculator](../.gitbook/assets/roi-calculator.png)

The **ROI Calculator** tab estimates the time and cost savings from using mirrord. Configure the inputs on the left and see the calculated results on the right:

**Inputs:**

* **Number of developers**: How many developers use mirrord
* **Developer cost**: Annual salary or hourly rate
* **Deploy/test cycles per developer per day**: How many code-deploy-test loops each developer does daily
* **Minutes per deploy cycle**: Time saved per cycle by using mirrord instead of deploying to the cluster
* **Infrastructure savings** (expandable): Number of staging environments decommissioned and their monthly or annual cost

**Results:**

* **Hours saved per developer per week**
* **Faster delivery** percentage
* **Annual productivity savings** (team hours and dollar value)
* **Annual infrastructure savings**
* **Net Annual ROI** after subtracting the mirrord license cost

## Features

### Dark Mode

Toggle between light and dark themes using the moon/sun icon in the top-right corner of the app bar. Your preference is saved in the browser's local storage.

### Manual Sync

Click the **Sync** button in the app bar to manually refresh all dashboard data. The last updated timestamp is displayed next to the button.

### Operator Version

The operator version is displayed in the app bar for quick reference (e.g., `v3.142.0`).
