# Workflow Structure & Options

This document outlines the three main ways the Windows build workflow operates in the new structure.

## 1. Standard PR / Push Flow

**Trigger**: Push to a branch or Open/Update a Pull Request.

In this mode, the Windows build runs as a **check** to ensure code validity. It **does not** sign artifacts or publish them.

```mermaid
%%{init: {'theme': 'neutral', 'themeVariables': {'lineColor': '#f00'}}}%%
graph TD
    A["User Pushes Code"] --> B["CI Workflow (ci.yaml)"]
    B --> C{"Windows Files Changed?"}
    C -->|Yes| D["Call Reusable<br>Windows Build"]
    C -->|No| E["Skip Windows Build"]
    D --> F["Restoring Cache / Building"]
    F --> G["Run Tests"]
    G --> H["Upload Artifacts (Unsigned)"]
    H --> I[End]
```

*   **Inputs**: Uses defaults (`sign_artifacts: false`, `choco_publish: false`).
*   **Permissions**: Inherits `read` permissions (safe for forks).

## 2. Release Process Flow

**Trigger**: Pushing a tag (e.g. `v3.12.0`).

In this mode, the Windows build is part of the official release pipeline. It **signs** the artifacts and **uploads** them to the GitHub Release.

```mermaid
%%{init: {'theme': 'neutral', 'themeVariables': {'lineColor': '#f00'}}}%%
graph TD
    A["Push Tag v*"] --> B["Release Workflow (release.yaml)"]
    B --> C["Build Linux & Mac Artifacts"]
    B --> D["Call Reusable<br>Windows Build"]
    
    subgraph "Windows Build (Reusable)"
        D --> D1["Build Artifacts"]
        D1 --> D2["Sign Artifacts (Certificate)"]
        D2 --> D3["Run Tests"]
        D3 --> D4{"Release Tag Provided?"}
        D4 -->|Yes| D5["Upload to GitHub Release"]
        D5 --> D6["Publish Chocolatey<br>/ WinGet"]
    end

    C --> E["Create GitHub Release"]
    D6 --> E
```

*   **Inputs**: `sign_artifacts: true`, `choco_publish: true`, `winget_publish: true`.
*   **Permissions**: Inherits `write` permissions (required for upload).

## 3. Manual / Standalone Flow

**Trigger**: Manually running the workflow via GitHub Actions UI.

This gives you full control. You can use this to test the release process without pushing a tag, or to build specific artifacts for debugging.

```mermaid
%%{init: {'theme': 'neutral', 'themeVariables': {'lineColor': '#f00'}}}%%
graph TD
    A["User - Actions Tab"] --> B["Select 'Windows Build'"]
    B --> C["Configure Inputs"]
    C --> D["Run Workflow<br>(workflow_<br>dispatch)"]
    
    D --> E{"Input: Sign?"}
    E -->|True| F["Sign Artifacts"]
    E -->|False| G["Skip Signing"]
    
    F & G --> H["Run Tests"]
    
    H --> I{"Input: Release Tag?"}
    I -->|Set| J["Upload to Release"]
    I -->|Empty| K["Skip Upload"]
```

*   **Inputs**: Fully configurable UI (Mode, Tag, Sign, Publish Choco, Publish WinGet).
*   **Permissions**: Uses `write` permissions by default (user triggered).
