import JsonHighlight from './JsonHighlight'

export default function ConfigTab({ config }: { config: unknown }) {
  return (
    <div className="h-full overflow-auto p-4">
      <JsonHighlight value={config} />
    </div>
  )
}
