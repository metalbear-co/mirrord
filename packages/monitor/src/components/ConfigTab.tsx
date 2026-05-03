import JsonHighlight from './JsonHighlight'

export default function ConfigTab({ config }: { config: unknown }) {
  return (
    <div className="p-4 overflow-auto h-full">
      <JsonHighlight value={config} />
    </div>
  )
}
