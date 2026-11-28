---
title: Use mirrord in your IDE
description: "Install the mirrord extension for VS Code or a JetBrains IDE, and run or debug your app against your Kubernetes cluster or Amazon ECS."
tags:
  - oss
  - team
  - enterprise
---

# Use mirrord in your IDE

The mirrord extension runs your app with mirrord when you run or debug it from your IDE, so you keep your launch configurations, breakpoints, and debugger.

You need a workload to run against: a deployment or a pod in your cluster, [another target](../reference/targets.md), or a [workload outside Kubernetes](../using-mirrord/beyond-kubernetes/README.md), such as an Amazon ECS task. For a Kubernetes workload, your machine and cluster need to meet the [requirements](install-mirrord.md#requirements). For a workload outside Kubernetes, see [Required versions](../using-mirrord/beyond-kubernetes/README.md#required-versions).

## Install

{% tabs %}
{% tab title="VS Code / Cursor / Windsurf" %}

Extensions → search **mirrord** → Install.

Works with all VS Code forks (Cursor, Windsurf, Antigravity, PearAI, Trae). You can also download it from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

A **mirrord** button with a circle appears in the status bar at the bottom of the window.

{% endtab %}

{% tab title="JetBrains" %}

Preferences → Plugins → search **mirrord** → Install.

Works with IntelliJ, GoLand, PyCharm, and other JetBrains IDEs. You can also download it from the [JetBrains Marketplace](https://plugins.jetbrains.com/plugin/19772-mirrord).

The mirrord icon appears in the navigation toolbar, top right.

{% hint style="info" %}
On Windows, some run configurations still need WSL. See [JetBrains IDEs](../installing-mirrord/intellij.md) for the ones that run natively.
{% endhint %}

{% endtab %}
{% endtabs %}

The extension downloads the mirrord binary it needs. To run `mirrord` commands yourself, such as `mirrord ui`, the [Onboarding Wizard](onboarding-wizard.md), or the [mirrord MCP Server](../using-mirrord-with-ai/mcp-server.md), [install the CLI](install-mirrord.md) too.

## Run your first session

{% tabs %}
{% tab title="VS Code / Cursor / Windsurf" %}

1. Click **mirrord** in the status bar. The circle fills in when mirrord is enabled.

   ![mirrord button](<../.gitbook/assets/enabler (1).png>)

2. Run or debug your app as you usually do.
3. Pick the target to run against. To see targets in another namespace, pick **Select Another Namespace**.

{% endtab %}

{% tab title="JetBrains" %}

1. Click **Enable/Disable mirrord** in the navigation toolbar, next to the **mirrord** menu.

   ![Enable/Disable mirrord button](../.gitbook/assets/enabler.png)

2. Run or debug your app as you usually do.
3. Pick the target to run against.

For some projects, such as Java projects that use the IntelliJ build system, the plugin can't show the target dialog and cancels the run. Set the target in your config file instead. See [Selecting session target](../installing-mirrord/intellij.md#selecting-session-target).

{% endtab %}
{% endtabs %}

The target list shows Kubernetes workloads only. To run against a workload outside Kubernetes, set its `serverless/` target in a config file instead. See [From your IDE](../using-mirrord/beyond-kubernetes/README.md#from-your-ide). With that config, only requests that carry the `baggage: mirrord-session=<YOUR_SESSION_KEY>` header reach your local process.

{% hint style="success" %}
**Send a request to your remote target.** You should see it arrive at your local process as well!
{% endhint %}

To see what mirrord does by default, see [What just happened?](first-session.md#what-just-happened)

## Use a config file

To change what mirrord does, add a `mirrord.json` to your project's `.mirrord` directory, or pick a config with **Select active config** in the mirrord menu. The extension autocompletes `mirrord.json` files. For the order in which the extension looks for a config, see [VS Code](../installing-mirrord/vscode.md#using-the-mirrord-config) or [JetBrains IDEs](../installing-mirrord/intellij.md#using-the-mirrord-config).

## Next steps

- [VS Code](../installing-mirrord/vscode.md) and [JetBrains IDEs](../installing-mirrord/intellij.md): launch configurations, target selection, config files, and the mirrord binary.
- [Filtering Incoming Traffic](../using-mirrord/incoming-traffic/filter-incoming-traffic.md): steal only your own requests, so your local process answers them instead of the deployed service.
- [Install the Operator](install-the-operator.md): for teams sharing a cluster, the platform team installs it once per cluster.
