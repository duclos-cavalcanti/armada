import '@xyflow/react/dist/style.css'
import './theme.css'
import Topology from './Topology'

// Baseline: the canonical architecture topology only. Flow overlays and the
// earlier per-flow views were removed to iterate on the topology first.
export default function App() {
  return <Topology />
}
