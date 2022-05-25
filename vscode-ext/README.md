<p align="center">
  <img src="images/icon.png">
</p>
<h1 align="center">mirrord</h1>

VS Code extension of [mirrord](https://mirrord.dev)

mirrord lets you easily mirror traffic from your Kubernetes cluster to your local service. For more info on mirrord, check our [docs](https://mirrord.dev)


## Getting Started
Once mirrord extension is installed, you will have an enable/disable mirrord button on the "status bar" (bottom of screen usually). Once enabled, on debug launch of process, it will let you select which pod to work with. (mirror traffic from)

## Settings

- You can control the namespace mirrord will find pods by changing the impersonated pod namespace by clicking the settings button next to the Enable/Disable mirrord button
- You can also control in which k8s namespace the mirrord-agent will spawn using the same setting button.
