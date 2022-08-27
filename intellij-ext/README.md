# mirrord

<!-- Plugin description -->
mirrord works by letting you select a pod to mirror traffic from. It launches a privileged pod on the same node which enters the namespace of the selected pod and captures traffic from it.
### How To Use

* Click "Enable/Disable mirrord" toggle button on the run tool window.
* Start debugging your project
* Choose pod to mirror traffic from, select and configure mirrord options.
* The debugged process will start with mirrord, and receive traffic.
 
<!-- Plugin description end -->

## Installation

- Using IDE built-in plugin system:
  
  <kbd>Settings/Preferences</kbd> > <kbd>Plugins</kbd> > <kbd>Marketplace</kbd> > <kbd>Search for "mirrord-intellij-plugin"</kbd> >
  <kbd>Install Plugin</kbd>
  
- Manually:

  Download the latest release and install it manually using
  <kbd>Settings/Preferences</kbd> > <kbd>Plugins</kbd> > <kbd>⚙️</kbd> > <kbd>Install plugin from disk...</kbd>


---
Plugin based on the [IntelliJ Platform Plugin Template][template].

[template]: https://github.com/JetBrains/intellij-platform-plugin-template
