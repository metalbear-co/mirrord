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

- You can control the namespace mirrord will find pods by changing the impersonated pod namespace by clicking the settings button next to the Enable/Disable mirrord button
- You can also control in which k8s namespace the mirrord-agent will spawn using the same setting button.
- You can control which container to impersonate within the impersonated pod
- Different functionality can be turned on or off: outgoing traffic, file operations, environment variables, and DNS.