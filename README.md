<p align="center">
  <img src="images/icon.png">
</p>
<h1 align="center">mirrord</h1>

mirrord lets you easily mirror traffic from your Kubernetes cluster to your development environment. It comes as both [Visual Studio Code](https://code.visualstudio.com/) extension and a CLI tool.


## Getting Started
- [VSCode Extension](#vscode-extension)
- [CLI Tool](#cli-tool)
> mirrord uses your machine's default kubeconfig for access to the Kubernetes API.
---
## VSCode Extension
### Installation
Get the extension [here](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

### How to use

* Start debugging your project
* Click "Start mirrord" on the status bar
* Choose pod to mirror traffic from
* To stop mirroring, click "Stop mirrord" (or stop debugging)

<p align="center">
  <img src="https://i.imgur.com/LujQb1u.gif" width="738">
</p>

Currently the extension only captures port 80, but that will be configurable soon.
The extension automatically detects which port your debugged process listens on and directs the mirrored traffic to it.
If you prefer to direct traffic to a different local port, edit launch.json:

`{
  "mirrord": {
                "port": "<port to send traffic to>"
            }
}`

---
## CLI Tool
### Installation
`npm install -g mirrord`

### How to use
`mirrord <pod name>`

<p align="center">
  <img src="https://i.imgur.com/EgyBxI9.gif" width="538">
</p>

For more options, run:

`mirrord --help`

---
## How it works
mirrord works by letting you select a pod to mirror traffic from. It launches a privileged pod on the same nodewhich enters the namespace of the selected pod and captures traffic from it.



For more technical information, see [TECHNICAL.md](./TECHNICAL.md)

### Caveats
* mirrord currently supports Kubernetes clusters using containerd runtime only. Support for more runtimes will be added if there's demand.




## Contributing
Contributions are welcome via PRs.


## Help & Community 🎉✉️

Join our [Discord Server](https://discord.gg/J5YSrStDKD) for questions, support and fun. 

---


<i>Icon Credit: flaticon.com</i>
