---
title: Onboarding wizard
date: 2026-01-05T12:00:00.000Z
lastmod: 2026-01-05T12:00:00.000Z
draft: false
toc: true
description: Using the mirrord wizard to speed up onboarding setup
tags:
  - oss
  - team
  - enterprise
---

# Onboarding Wizard

The mirrord onboarding wizard is designed to make getting started with mirrord easier and faster. It does this by taking you through the basics of mirrord and some commonly used features, and then lets you create and download your first mirrord configuration file via the UI.

{% hint style="info" %}
This feature is available in mirrord version 3.178.0 and onwards.
{% endhint %}

### Launching the wizard

Starting the wizard will open a locally hosted webapp in your browser. To launch the wizard, install mirrord and run the following command:

```bash
mirrord wizard
```

Make sure that you are connected (and if necessary signed in) to your cluster and that if you are using the operator, it is set up properly. If you do not, the wizard will not be able to list targets or suggest ports for network configuration.

![The Onboarding Wizard Homepage](../.gitbook/assets/wiz-home.png)

#### Learning overview

As a new user, you may want to learn some mirrord basics before jumping into creating a configuration file. You can do this from the wizard landing page, by leafing through the overview before getting to the configuration options.

### Creating a configuration

There are four parts to configuration creation:

1. Boilerplate selection
2. Target configuration
3. Network configuration
4. Export

#### Boilerplate selection

![Wizard Boilerplate Selection](../.gitbook/assets/wiz-boiler.png)

There are three options for boilerplate configurations that you can choose between. The differences between them are explained in more detail in the overview steps.

_Note that using "Replace" mode requires the operator (not available in OSS)_.

#### Target configuration

![Wizard Target Selection](../.gitbook/assets/wiz-target.png)

Select a target from a given namespace in your cluster, optionally filtering by specific types (this can be helpful if there are a lot of resources in your cluster). Target selection is required in the wizard (targetless configurations are not yet supported).

If there are namespaces or resources missing, it could be that there is an issue with your connection to the cluster, or that the resources require the operator and you are using OSS.

#### Network configuration

Optionally configure traffic filters and/or ports.

**Traffic filters**

![Wizard Traffic Filter Configuration](../.gitbook/assets/wiz-filter.png)

Here, you can set header or path filters for traffic. Strings are RegEx by default, but selecting "Exact" matching from the dropdown will enable exact matching instead.

If you set multiple filters, you can choose how to combine them: "Any" will filter traffic that matches one or more of the filters (logical OR), whereas "All" will filter traffic that matches every filter (logical AND).

**Ports**

![Wizard Port Configuration](../.gitbook/assets/wiz-port.png)

Here, you can configure specific ports that you are interested in mirroring or stealing. Ports that are exposed in the resource definition of your target will be added automatically, but you can remove these or add your own.

Additionally, if you wish to map ports that differ on the local and remote, you can do so by editing the remote port for the corresponding entry in the list.

#### Export

![Wizard Configuration Export](../.gitbook/assets/wiz-export.png)

Preview and download (or copy to clipboard) the configuration file according to the options you have set in the previous steps. To use a configuration file in the CLI, use the `-f <CONFIG_PATH>` flag. Or if using VSCode Extension or JetBrains plugin, simply create a `.mirrord/mirrord.json` file or use the UI.
