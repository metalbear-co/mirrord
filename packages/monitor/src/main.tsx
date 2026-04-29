import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import { autoConfigureExtension } from './extensionConfigure'
import '@metalbear/ui/styles.css'
import './index.css'

autoConfigureExtension()

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
