export const strings = {
  errorBoundary: {
    title: 'Something went wrong',
  },
  homepage: {
    title: 'Configuration Wizard',
    generatePrefix: 'Generate a',
    configFileName: 'mirrord.json',
    generateSuffix:
      'config file to connect your local environment to Kubernetes.',
    getStarted: 'Get Started',
    or: 'or',
    bookEmoji: '📖',
    learnBasics: 'New to mirrord? Learn the basics first',
  },
  wizard: {
    dialogDescription: 'mirrord configuration wizard',
    back: 'Back',
    continue: 'Continue',
    next: 'Next',
  },
  addNewFilter: {
    exactMatch: 'Exact Match',
    regexMatch: 'Regex Match',
    add: 'Add',
  },
  boilerplateStep: {
    heading: 'How do you want to interact with remote traffic?',
    subheading: 'Choose a mode that fits your development workflow',
    recommended: 'Recommended',
  },
  configTabs: {
    tabTarget: 'Target',
    tabNetwork: 'Network',
    tabExport: 'Export',
    exportTitle: 'Export Configuration',
    exportSubtitle: 'Review and export your mirrord.json configuration',
    configurationJson: 'Configuration JSON',
    copyToClipboard: 'Copy to Clipboard',
    downloadFile: 'Download File',
    howToUse: 'How to use your configuration:',
    cliLabel: 'CLI:',
    cliUse: 'Use the',
    cliFlag: '-f <CONFIG_PATH>',
    flag: 'flag',
    ideLabel: 'VSCode / JetBrains:',
    ideCreate: 'Create a',
    ideFileName: '.mirrord/mirrord.json',
    file: 'file',
    seeThe: 'See the',
    configDocs: 'configuration documentation',
    forMoreDetails: 'for more details.',
  },
  learningSteps: {
    whatIsLead: 'mirrord',
    whatIsBody:
      'lets you run your local process in the context of a Kubernetes cluster. Instead of deploying your code to test it, you can develop and debug locally while connected to your cloud environment.',
    whatIsBody2:
      'This means faster development cycles, real environment testing, and no need to set up complex local infrastructure.',
    howBody:
      'mirrord intercepts system calls from your local process and forwards them to a remote pod in your Kubernetes cluster.',
    howBody2:
      'Your local process sees the remote file system, environment variables, and network traffic as if it were running in the cluster.',
    modesBody: 'mirrord supports three ways to handle incoming traffic:',
    modeMirror: 'Mirror',
    modeMirrorDesc: '— Copy traffic without affecting the remote service',
    modeSteal: 'Steal (Filter)',
    modeStealDesc: '— Redirect specific traffic based on headers or paths',
    modeReplace: 'Replace',
    modeReplaceDesc:
      '— Fully replace the remote service with your local process',
    loopBody: 'With mirrord, your development workflow becomes:',
    loopStepOne: '1.',
    loopStepOneText: 'Write code locally',
    loopStepTwo: '2.',
    loopStepTwoText: 'Run with mirrord to test in cluster context',
    loopStepThree: '3.',
    loopStepThreeText: 'Debug and iterate instantly',
    configBody:
      'mirrord uses a JSON configuration file to define how it connects to your cluster and handles traffic.',
    configExample: `{
  "target": {
    "path": "deployment/my-app",
    "namespace": "default"
  },
  "feature": {
    "network": {
      "incoming": { "mode": "steal" }
    }
  }
}`,
    configBody2:
      'This wizard will help you create this configuration file step by step.',
    readyTitle: 'You now understand the basics of mirrord!',
    readyBody:
      'Let\'s create your configuration file. Click "Start Configuration" to begin selecting your target and traffic mode.',
    previous: 'Previous',
    skip: 'Skip',
  },
  networkTab: {
    incomingTitle: 'Incoming Traffic',
    incomingSubtitle: 'Configure how incoming traffic is handled',
    filteringTitle: 'Traffic Filtering',
    filteringSubtitle:
      'Steal a subset of traffic by specifying HTTP header or path filters',
    headerFilters: 'Header Filters',
    pathFilters: 'Path Filters',
    filterLogic: 'Filter Logic',
    filterAll: 'All',
    filterAllDesc: '- Match all specified filters',
    filterAny: 'Any',
    filterAnyDesc: '- Match any specified filter',
    portConfiguration: 'Port Configuration',
    portsDetected: 'ports were detected automatically in the target.',
    remotePort: 'Remote Port',
    localPort: 'Local Port',
    addNewPort: 'Add New Port',
  },
  targetTab: {
    title: 'Target Selection',
    subtitle: 'Choose the Kubernetes resource to connect to',
    kubeContext: 'Kube Context',
    kubeContextsError: "Couldn't load your Kubernetes contexts",
    kubeContextsErrorHint:
      'Make sure your kubeconfig is reachable: set KUBECONFIG or MIRRORD_KUBECONFIG to its path, then run "mirrord ui stop" and open the wizard again.',
    namespace: 'Namespace',
    noNamespaces: 'No namespaces available',
    resourceType: 'Resource Type',
    allTypes: 'All types',
    target: 'Target',
    selectTarget: 'Select a target...',
    loadingTargets: 'Loading targets...',
    noTargets: 'No targets found',
    selectTargetPrompt: 'Please select a target to continue',
    container: 'Container',
    noContainers: 'No containers found',
  },
} as const
