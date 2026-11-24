// The question wording matches `mirrord up init` (`mirrord/up/src/init.rs`); keep them in sync.
// Mode texts come from the server.
export const strings = {
  errorBoundary: {
    title: 'Something went wrong',
  },
  homepage: {
    title: 'Configuration Wizard',
    generatePrefix: 'Generate a',
    configFileName: 'mirrord-up.yaml',
    generateSuffix:
      'file to run your local services against your Kubernetes cluster.',
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
    addService: 'Add Service',
    stepTitles: {
      learning: 'Learn mirrord',
      mode: 'Choose Mode',
      target: 'Choose Target',
      traffic: 'Traffic',
      run: 'Local Command',
      export: 'Export',
    },
  },
  modeStep: {
    heading: 'How do you want to interact with remote traffic?',
    recommended: 'Recommended',
    loading: 'Loading modes...',
    failed: 'Could not load the modes from mirrord.',
  },
  targetStep: {
    title: 'Target Selection',
    subtitle: 'Choose the Kubernetes workload this service runs against',
    kubeContext: 'Kube Context',
    unknownContext: 'Current context',
    namespace: 'Select a namespace:',
    noNamespaces: 'No namespaces available',
    resourceType: 'Resource Type',
    allTypes: 'All types',
    workload: 'Select a workload',
    selectTarget: 'Select a workload...',
    searchTargets: 'Search workloads...',
    loadingTargets: 'Loading workloads...',
    noTargets: 'Found no workloads that you can list in this namespace.',
    targetless: 'Run without a target (outgoing traffic only)',
    serviceName: 'Service name:',
  },
  trafficStep: {
    title: 'Traffic',
    subtitle: 'Choose which requests and ports reach your local process',
    headerFilter:
      'HTTP header filter (regex; blank for auto session-key filter):',
    headerFilterHelp: 'Example: `session-id: my-session-identifier`',
    interceptPorts: 'Ports to intercept (detected on the target):',
    interceptPortsHelp:
      'The detected ports that you do not select are ignored.',
    detectingPorts: 'Detecting the ports of the target...',
    noPortsDetected: 'No ports were detected on the target.',
    ignorePorts: 'Ports to ignore (comma-separated, blank for none):',
    otherIgnorePorts:
      'Other ports to ignore (comma-separated, blank for none):',
    // Also linked from `mirrord up init` (`MODES_DOCS_URL` in `mirrord/up/src/init/modes.rs`).
    modesDocs: 'Learn more',
    modesDocsUrl:
      'https://metalbear.com/mirrord/docs/using-mirrord/multiple-concurrent-sessions/#service-modes',
  },
  runStep: {
    title: 'Local Command',
    subtitle: 'Choose how mirrord up starts this service on your machine',
    command: 'Local command (e.g. `go run ./cmd/api`):',
    directory: 'Working directory (blank for the current directory):',
    directoryHelp:
      'Relative to the directory of mirrord-up.yaml. Set it when the service is in a subdirectory, e.g. in a monorepo.',
  },
  exportStep: {
    title: 'Export Configuration',
    subtitle: 'Review and export your mirrord-up.yaml',
    services: 'Services',
    noTarget: 'no target',
    removeService: 'Remove service',
    addAnother: 'Add another service',
    yaml: 'mirrord-up.yaml',
    rendering: 'Rendering...',
    copyToClipboard: 'Copy to Clipboard',
    downloadFile: 'Download File',
    fileName: 'mirrord-up.yaml',
    howToUse: 'How to use your configuration:',
    cliLabel: 'CLI:',
    cliRun: 'Run',
    cliCommand: 'mirrord up -f <file>',
    seeThe: 'See the',
    docs: 'mirrord up documentation',
    docsUrl:
      'https://metalbear.com/mirrord/docs/using-mirrord/multiple-concurrent-sessions/',
    forMoreDetails: 'for more details.',
    // `mirrord up init` does not ask for `common` settings (good defaults), so only point to them.
    commonLabel: 'Shared settings:',
    commonPrefix: 'Settings for every service, like',
    commonOperator: 'operator',
    commonAnd: 'or',
    commonTelemetry: 'telemetry',
    commonGoUnder: ', go under',
    commonBlock: 'common',
    commonDocs: 'See the common settings',
    period: '.',
    commonDocsUrl:
      'https://metalbear.com/mirrord/docs/using-mirrord/multiple-concurrent-sessions/#common',
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
    loopBody: 'With mirrord, your development workflow becomes:',
    loopStepOne: '1.',
    loopStepOneText: 'Write code locally',
    loopStepTwo: '2.',
    loopStepTwoText: 'Run with mirrord to test in cluster context',
    loopStepThree: '3.',
    loopStepThreeText: 'Debug and iterate instantly',
    configBody:
      'mirrord up reads a mirrord-up.yaml file that lists your services: the workload each one runs against, how it shares traffic, and the local command that starts it.',
    configExample: `services:
  api:
    target:
      path: deployment/api
    run:
      command: ["npm", "start"]`,
    configBody2:
      'This wizard will help you create this file step by step. Run it with mirrord up -f mirrord-up.yaml.',
    readyTitle: 'You now understand the basics of mirrord!',
    readyBody:
      'Let\'s create your mirrord-up.yaml. Click "Start Configuration" to choose a mode and a target for your first service.',
    previous: 'Previous',
    skip: 'Skip',
  },
} as const
