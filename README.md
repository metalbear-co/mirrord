<p align="center">
  <img src="images/icon.png">
</p>
<h1 align="center">mirrord</h1>

A [Visual Studio Code](https://code.visualstudio.com/) extension that lets you easily mirror traffic from your Kubernetes cluster to your development environment.

When you start debugging, mirrord will prompt you to select a pod to mirror traffic from. It will then mirror all traffic from that pod to the process you're debugging.


## Installation
Get the extension [here](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

## How to use

* Start debugging your project
* Click "Start mirrord" on the status bar
* Choose pod to mirror traffic from
* To stop mirroring, click "Stop mirrord" (or stop debugging)

<p align="center">
  <img src="https://i.imgur.com/LujQb1u.gif" width="738">
</p>

### Note
mirrord uses your machine's default kubeconfig for access to the Kubernetes API.

## How it works
mirrord works by letting you select a pod to mirror traffic from. It launches a privileged pod on the same node
which enters the namespace of the selected pod and captures traffic from it.

Currently it captures only port 80, but that will be configurable soon.
The extension automatically detects which port your debugged process listens on and directs the mirrored traffic to it.
If you prefer to direct traffic to a different local port, edit launch.json:

`{
  "mirrord": {
                "port": "<port to send traffic to>"
            }
}`

For more technical information, see [TECHNICAL.md](./TECHNICAL.md)

### Caveats
* mirrord currently supports Kubernetes clusters using containerd runtime only. Support for more runtimes will be added if there's demand.




## Contributing
Contributions are welcome via PRs.

---


<i>Icon Credit: flaticon.com</i>
