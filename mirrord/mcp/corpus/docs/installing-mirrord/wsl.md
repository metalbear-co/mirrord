---
title: WSL (deprecated)
description: Installing and using mirrord on Windows with WSL
---

# WSL (deprecated)

{% hint style="info" %}
mirrord now runs natively on Windows — no WSL needed. See the [CLI](cli.md), [VS Code](vscode.md), and [JetBrains IDEs](intellij.md) installation guides. This page is kept for users who still prefer to run mirrord through WSL, or for the few JetBrains run configurations that don't support native Windows yet.
{% endhint %}

Another way to run mirrord on Windows is using the _Linux Subsystem for Windows_ (_WSL_). You’ll also need a Kubernetes cluster. If you don’t have one, you can set one up locally using [Docker Desktop](https://docs.docker.com/desktop/install/windows-install/). mirrord works with any Kubernetes cluster, be it remote or local.

### Setting up WSL

You can read about the prerequisites and installation options on the official Microsoft documentation for [How to install Linux on Windows with WSL](https://learn.microsoft.com/en-us/windows/wsl/install).

The mirrord guide uses the default installation options, which has Ubuntu as the Linux distro. mirrord itself is not limited to any particular distro.

#### From the Microsoft Store

To install WSL from the Microsoft Store just open the Microsoft Store app, then search for the name of the Linux distro you want. We recommend installing Ubuntu, but mirrord works with any Linux distro.

After installation is complete, click on the Open button and a terminal window will appear.

#### From the Terminal

Open a terminal with **administrator privileges**.

* It doesn’t have to be the Windows Terminal. PowerShell and Command Prompt will also work.

On the terminal, run the `wsl –install` command to install the default (Ubuntu) Linux distro:

```powershell
wsl --install
```

* You can read more about the prerequisites and installation options on the official Microsoft documentation for [How to install Linux on Windows with WSL](https://learn.microsoft.com/en-us/windows/wsl/install). This guide uses the default installation options, which has Ubuntu as the Linux distro. mirrord itself is not limited to any particular distro.

After installing WSL, in a terminal window, you should see the following output from executing the `wsl --list` command:

```powershell
C:\> wsl --list
Windows Subsystem for Linux Distributions:
Ubuntu (Default)
```

* If you're not seeing any Linux Distribution listed, please refer back to the [Microsoft guide](https://learn.microsoft.com/en-us/windows/wsl/install), or join our [Slack community](https://metalbear.com/slack) and we'll be happy to help you.

To start a session in WSL, now enter the `wsl` command:

```powershell
wsl
```

#### Setting up the Linux distro

After starting a new WSL session (either from the command line, or from the Microsoft Store) you’ll be prompted to set up a Linux user. The username and password does not need to match your Windows user.

After setting up your Linux user, it’s time to prepare the Linux environment for development. Install the tools needed to access your Kubernetes cluster (gcloud cli, azure cli, or whatever cli tool you use for authentication and cluster connection). You’ll also need to install the compilers and project management tools (such as nvm, JDK, dotnet cli) necessary to run and debug your project.

* Many of those tools may be installed using the Linux distro package manager, but some might require manual installation and setup.

Some IDEs may support running in WSL from Windows directly (the IDE is installed on Windows), such as VS Code and the IntelliJ family of IDEs, while others may require being installed in Linux itself.

### Kubernetes on WSL

* Setting up a Kubernetes cluster is out of scope for this guide - we’re assuming that you have a remote cluster to target with mirrord. If you don’t have a Kubernetes cluster to use and still want to try out mirrord, we recommend checking out the Docker Desktop guide on [Install Docker Desktop on Windows](https://docs.docker.com/desktop/install/windows-install/).

With the tooling out of the way, and after cluster authorization has been set up, you may check cluster access with `kubectl get all`.

```sh
username@hostname:/mnt/c$ kubectl get all

NAME             	          TYPE    	    CLUSTER-IP   EXTERNAL-IP   PORT(S)   AGE
service/kubernetes   ClusterIP   10.96.0.1	     <none>    	       443/TCP    1d
```

* If you got a `command not found` error instead, this means that `kubectl` is not installed. Some Kubernetes tools install it as part of their setup, but you can also manually install it directly, follow the [official guide](https://kubernetes.io/docs/tasks/tools/install-kubectl-linux/) for installing it on Linux. You can also [install it on Windows](https://kubernetes.io/docs/tasks/tools/install-kubectl-windows/), but this may require changing the `KUBECONFIG` environment variable.
* If you’re not seeing any of your Kubernetes resources, you might need to change your Kubernetes configuration. Refer to the [`kube config` manual](https://kubernetes.io/docs/reference/kubectl/generated/kubectl_config/) manual.

### Running a project on WSL

Before starting your IDE, it’s recommended that you copy your project files from the Windows file system to Linux, to avoid performance issues. The best practice is to have everything inside Linux.

You can do this from the command line (from within Linux, the Windows file system should be something like `/mnt/{windows-drive-letter}`, so by default it’ll be `/mnt/c`), or from File Explorer.

#### Creating a playground project

* If you already have your own project, you may [skip this section](wsl.md#root-project-vscode).

We’ll provide you with a small playground project here, if you don’t already have your own. Let's create a sample NodeJS project to use with mirrord, but bear in mind that mirrord is not limited to any programming languages or frameworks. In the Linux terminal, navigate to the `home` directory.

```sh
cd ~
```

Create a new `playground` directory.

```sh
mkdir playground && cd playground
```

Install NodeJS (if you haven’t already in the Setting up the Linux Distro section). First update the package manager.

```sh
sudo apt update
```

Now install the nodejs package.

```sh
sudo apt install nodejs
```

Create a very simple NodeJS program.

```sh
echo "console.log('Hello, mirrord');" > app.mjs
```

Running `node app.mjs` should look something like this.

```sh
username@hostname:~/playground$ node app.mjs
Hello, mirrord
```

We can finally move on to installing and using mirrord.

#### Using mirrord in VS Code

* Microsoft provides a very good guide on [how to use WSL with VS Code](https://learn.microsoft.com/windows/wsl/tutorials/wsl-vscode).

Open VS Code from Windows, as you normally would, and click on the Remote Explorer.

Select the Linux distro you have set up, and click on the Connect in Current Window button that appears.

VS Code will notify you it’s starting WSL, and the Remote Explorer will change to indicate you’re connected.

Now go to the Extensions panel, search for mirrord and install it.

* Some of your extensions may appear as disabled, with a button to `Install in WSL`. If you want to use these extensions from the WSL VS Code, then you must click the button and install them.
* If you get an error saying that mirrord does not support the Windows platform, this means that you’re trying to install it on the Windows VS Code. Uninstall the mirrord extension, and follow the previous steps to start the WSL VS Code.

With mirrord installed, open up your project.

* Keep in mind that you’ll be navigating the directories with Linux style paths. If you have not copied your project files to WSL, you can navigate the Windows files from the `/mnt` directory.

#### Using mirrord in IntelliJ

* Jetbrains provides a very good guide on [how to use WSL with IntelliJ](https://www.jetbrains.com/help/idea/how-to-use-wsl-development-environment-in-product.html).

Open the Jetbrains IDE you have installed on Windows (the mirrord plugin is available for every Jetbrains IDE. In this tutorial we’ll show screen caps from IntelliJ Idea Ultimate, but that’s **not** a requirement).

Select the WSL option under Remote Development.

Click on the + button (if you already have a project, otherwise select New Project).

* Pay attention to the IDE version you’re choosing. The recommendation here is to select the same one that you have installed on Windows, pay close attention to the version numbers as well (sometimes the _Beta_ version comes selected by default).

Either type the path to your project, or click on the `...` button to open the path picker.

Now click `Download IDE and Connect` at the bottom.

The IDE will be downloaded and installed on Linux. After it’s ready, it should automatically open.

Click on the gear button, select `Plugins` and search the `Marketplace` for “mirrord”.

After clicking to install it, the install button will change to `Restart IDE`. Instead of restarting it like that, close the WSL IDE, and in the Windows IDE select to open your project again.

* If you get an error saying that mirrord does not support the Windows platform, this means that you’re trying to install it on the Windows IDE. Uninstall the mirrord extension, and follow the previous steps to start the WSL IDE.

#### Using mirrord from the CLI

In your WSL terminal, you can download and install mirrord by running the following command:

```sh
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash
```

* You might get prompted to enter your `root` user password, so we can install it in `/usr/local/bin`.
* If `curl` is not installed in the Linux distro, you can use the distro package manager to install it, or download and install it manually from the [curl website](https://curl.se/download.html).

Now to execute your project with mirrord, just run the `mirrord exec` command:

```sh
mirrord exec --target "<pod-target>" <process command> 
```

* If you’re using this guide’s playground project your `mirrord exec` command should be:

```sh
mirrord exec --target “targetless” node app.mjs
```

* You can list the available mirrord targets with the `mirrord ls` command. If no targets are being shown, you might not have any Kubernetes resources that can be targeted by mirrord, or you might not be using the right Kubernetes context. You can check the later with `kubectl config view`, look at the `current-context` and see if it’s the intended one. You may change the context with the `kubectl config use-context [CONTEXT NAME]` command.

You can use `mirrord exec –help` to list other `exec` options.

### Troubleshooting

#### IntelliJ failed to update mirrord binary

If you're seeing a mirrord notification pop-up that says something along the lines of:

> failed to update the mirrord binary: PKIX path building failed: sun.security.provider.certpath.SunCertPathBuilderException: unable to find valid certification path to requested target

Something is wrong with the local certificate that IntelliJ is trying to use. You can read more about this on the [IntelliJ certificates manual installation](https://www.jetbrains.com/help/idea/ssl-certificates.html#manual_configuration) page.

You can fix this issue by navigating to the IntelliJ IDE dir (change it to match where your IntelliJ IDE is installed) in the WSL terminal:

```sh
cd ~/.local/share/JetBrains/Toolbox/apps/{NAME-OF-IDE}/jbr/lib/security
```

And issuing the following command:

```sh
keytool -importcert -trustcacerts -alias <alias-name> -file <path/to/file.crt> -keystore cacerts
```

#### IntelliJ mirrord does not launch and settings `Select Active` does not work

If you don't see the mirrord UI for selecting the active configuration when clicking `Select Active`, this usually means that the mirrord plugin has been installed only on one side (host or client). This issue might also prevent mirrord from running inside WSL.

To fix it, make sure that the mirrord plugin is installed on both the host and the client (WSL) IDE. Open the host IDE (or client IDE), navigate to the Plugins section, and install mirrord.
