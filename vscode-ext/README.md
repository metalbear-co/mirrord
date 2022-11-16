<p align="center">
  <img src="images/icon.png" width="20%">
</p>
<h1 align="center">mirrord</h1>

mirrord lets developers run local processes in the context of their cloud environment. It’s meant to provide the benefits of running your service on a cloud environment (e.g. staging) without actually going through the hassle of deploying it there, and without disrupting the environment by deploying untested code. It comes as a Visual Studio Code extension, an IntelliJ plugin and a CLI tool. You can read more about it [here](https://mirrord.dev/docs/overview/introduction/).

## How to use

* Click "Enable mirrord" on the status bar
* Start debugging your project
* Choose pod to impersonate
* The debugged process will start with mirrord, and receive the context of the impersonated pod. It will receive its environment variables and incoming traffic, will read and write files to it, and send outgoing traffic through it.

<p align="center">
  <img src="https://i.imgur.com/FFiir2G.gif" width="60%">
</p>

> mirrord uses your machine's default kubeconfig for access to the Kubernetes API.

> For incoming traffic, make sure your local process is listening on the same port as the remote pod.
## Settings

mirrord reads its configuration from the following file: `<project-path>/.mirrord/mirrord.json`. You can also prepend a prefix, e.g. `my-config.mirrord.json`, or use .toml or .yaml format.
It's documented [here](https://mirrord.dev/docs/overview/configuration/). It also supports autocomplete when edited in VS Code.
