---
title: Installing mirrord
description: How to install mirrord on your local machine
---

mirrord can be installed and used in several ways depending on your development workflow:

- **[CLI](cli.md)** - Install the mirrord command-line tool directly
- **[VS Code](vscode.md)** - Install the mirrord extension for Visual Studio Code and compatible editors (Cursor, Windsurf, etc.)
- **[JetBrains IDEs](intellij.md)** - Install the mirrord plugin for JetBrains IDEs (IntelliJ, PyCharm, GoLand, etc.)

mirrord runs natively on Windows — no WSL needed. If you still prefer to use WSL, see our **[WSL setup guide](wsl.md)**.

## Local Requirements

For your local machine, you may use any of:
- MacOS (Intel, Apple Silicon)
- Linux (x86_64)
- Windows (x86_64), WSL (x86_64)

kubectl needs to be configured on the local machine.

## mirrord Operator

If you're planning to use [mirrord for Teams](https://app.metalbear.com), you'll also need to install the mirrord Operator in your cluster. See the [mirrord Operator](../managing-mirrord/operator.md) page for installation instructions.
