<p align="center">
  <img src="images/icon.png" width="20%">
</p>
<h1 align="center">mirrord</h1>

mirrord lets you easily mirror traffic from your Kubernetes cluster to your local service. For more info on mirrord, check our [docs](https://mirrord.dev)

## How to use

* Click "Enable mirrord" on the status bar
* Start debugging your project
* Choose pod to mirror traffic from
* The debugged process will start with mirrord, and receive traffic 

<p align="center">
  <img src="https://i.imgur.com/FFiir2G.gif" width="60%">
</p>

> mirrord uses your machine's default kubeconfig for access to the Kubernetes API.

> Make sure your local process is listening on the same port as the remote pod.
## Settings

- You can control the namespace mirrord will find pods by changing the impersonated pod namespace by clicking the settings button next to the Enable/Disable mirrord button
- You can also control in which k8s namespace the mirrord-agent will spawn using the same setting button.
- You can control which container name in the impersonated pod namespace
