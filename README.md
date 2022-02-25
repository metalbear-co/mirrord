<p align="center">
  <img src="images/icon.png">
</p>
<h1 align="center">mirrord</h1>

A [Visual Studio Code](https://code.visualstudio.com/) extension that lets you easily mirror traffic from your Kubernetes cluster to your development environment.

When you start debugging, mirrord will prompt you to select a pod to mirror traffic from. It will then mirror all traffic from that pod to the process you're debugging.


## Installing
Get the extension [here](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

## How to use

* Start debugging your project.
* Click "Start mirrord" on the status bar.
* Choose pod to mirror traffic from.
* Click stop when you wish to finish (or stop debugging).

<p align="center">
  <img src="https://i.imgur.com/LujQb1u.gif" width="738">
</p>

## How it works
mirrord works by letting you select a pod to mirror traffic from. It launches a priveleged pod on the same node
which enters the namespace of the selected pod and captures traffic from it.

Currently it captures only port 80, but that will be changable from the extension soon.
The extension automatically detects which port your application listens on to redirect traffic to.
To manually choose a port, edit launch json:

`{
  "mirrord": {
                "port": "<port to send traffic to>"
            }
}`

For more technical information, see [TECHNICAL.md](./TECHNICAL.md)

### Caveats
* mirrord currently supports Kubernetes clusters using containerd runtime only. Support for more runtimes will arrive if demanded.


### Note
mirrord uses your machine's default kubeconfig for access to the Kubernetes API.


## Contributing
Contributions are welcome via PRs.

---


<i>Icon Credit: flaticon.com</i>
