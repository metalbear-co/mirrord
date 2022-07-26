<div align="center">

![mirrord logo dark](./images/logo_dark.png#gh-dark-mode-only)

</div>

<div align="center">

![mirrord logo light](./images/logo_light.png#gh-light-mode-only)

</div>

[![Discord](https://img.shields.io/discord/933706914808889356)](https://discord.gg/J5YSrStDKD)
![License](https://img.shields.io/badge/license-MIT-green)
![GitHub release (latest SemVer)](https://img.shields.io/github/v/release/metalbear-co/mirrord)
[![Twitter Follow](https://img.shields.io/twitter/follow/metalbearco?style=social)](https://twitter.com/metalbearco)

mirrord lets you easily mirror traffic from your Kubernetes cluster to your development environment. It comes as both a [Visual Studio Code](https://code.visualstudio.com/) extension and a CLI tool.

## Getting Started
- [VSCode Extension](#vscode-extension)
- [CLI Tool](#cli-tool)
> mirrord uses your machine's default kubeconfig for access to the Kubernetes API.

> Make sure your local process is listening on the same port as the remote pod.
---
## VSCode Extension
### Installation
Get the extension [here](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

### How To Use

* Click "Enable mirrord" on the status bar
* Start debugging your project
* Choose pod to mirror traffic from
* The debugged process will start with mirrord, and receive traffic

<p align="center">
  <img src="./images/demo.gif" width="60%">
</p>

---
## CLI Tool
### Installation
```sh
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash
```

* Windows isn't currently supported (you can use WSL)

### How To Use
```sh
mirrord exec <process command> --pod-name <name of the pod to impersonate>
```
e.g. 

```sh
mirrord exec node app.js --pod-name my-pod
```

---

## How It Works
mirrord works by letting you select a pod to mirror traffic from. It launches a privileged pod on the same node which enters the namespace of the selected pod and captures traffic from it.

## Contributing
Contributions are welcome via PRs.


## Help & Community 🎉✉️

Join our [Discord Server](https://discord.gg/J5YSrStDKD) for questions, support and fun.

## Code of Conduct
We take our community seriously and we are dedicated to providing a safe and welcoming environment for everyone.
Please take a few minutes to review our [Code of Conduct](./CODE_OF_CONDUCT.md).

## License
[MIT](./LICENSE)
