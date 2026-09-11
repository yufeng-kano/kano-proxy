import { createApplication, createWorker } from "./application"

export const app = createApplication()
export default createWorker(app)
export { AgentTunnel } from "./do/agent_tunnel"
