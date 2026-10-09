---
title: Limitations
date: 2020-11-16T12:59:39.000Z
lastmod: 2020-11-16T12:59:39.000Z
draft: false
images: []
menu:
  docs:
    parent: faq
weight: 120
toc: true
tags:
  - oss
  - team
  - enterprise
description: What are the limitations to using mirrord?
---

## What frameworks/languages does mirrord support?

mirrord works by [hooking libc](https://metalbear.com/blog/mirrord-internals-hooking-libc-functions-in-rust-and-fixing-bugs/), so it should work with any language/framework that uses libc (vast majority).

This includes: Rust, Node, Python, Java, Kotlin, Ruby, and others (most languages use libc).

mirrord also supports [Go](https://metalbear.com/blog/hooking-go-from-rust-hitchhikers-guide-to-the-go-laxy/), which doesn't use libc

## Does mirrord support clusters with a service mesh like Istio or Linkerd?

Yes, mirrord works exactly the same way with and without a service mesh installed.

## Does mirrord support OpenShift?

Yes, mirrord works with OpenShift. However, OpenShift usually ships with a default security policy that doesn't let mirrord create pods. To fix this, you would need to tweak your `scc` settings - more information [here](https://docs.openshift.com/container-platform/3.11/admin_guide/manage_scc.html). If you'd rather keep the default security policies, we recommend trying out [mirrord for Teams](../managing-mirrord/operator.md).

## Does mirrord support Teleport, Tailscale, or other secure cluster access tools?

Yes, and there's nothing to configure.

mirrord reaches your cluster entirely through the Kubernetes API, using the same kubeconfig `kubectl` uses. That covers everything it does: the [mirrord Operator](../managing-mirrord/operator.md) is exposed as a Kubernetes APIService, and the connection to the [mirrord agent](../reference/architecture.md#mirrord-agent) runs over Kubernetes port forwarding. mirrord never opens a network path of its own.

If `kubectl` works through your access provider, mirrord works through it too. Customers run mirrord behind Teleport and Tailscale today.

## Does mirrord support binaries that are statically compiled? (Linux)

No, mirrord needs to be able to leverage dynamic linking in order to work. This means static binaries are not supported.

To check a binary, you can use the `file <FILE_NAME>` command - dynamically linked binaries will look like this:

```bash
marvin@heart-of-gold:~$ file /usr/bin/ls
/usr/bin/ls: ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2, BuildID[sha1]=36b86f957a1be53733633d184c3a3354f3fc7b12, for GNU/Linux 3.2.0, stripped
```

And static binaries will look like this:

```bash
marvin@heart-of-gold:~/MetalBear$ file some_static_binary 
some_static_binary: ELF 64-bit LSB executable, x86-64, version 1 (GNU/Linux), statically linked, BuildID[sha1]=2e1eda62d5f755377435c009e856cd7b9836734e, for GNU/Linux 3.2.0, not stripped
```

Sometimes, Go is statically compiled by default, so it's important to check and compile dynamically if necessary. See [this section in Common Issues](../troubleshooting/common-issues.md#ive-run-my-program-with-mirrord-but-it-seems-to-have-no-effect) for more info.
