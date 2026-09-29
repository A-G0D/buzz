import { invokeTauri } from "@/shared/api/tauri";
import type {
  DeviceMemorySnapshot,
  GlobalAgentResourcePolicy,
} from "@/shared/api/types";

export async function getGlobalAgentResourcePolicy(): Promise<GlobalAgentResourcePolicy> {
  return invokeTauri<GlobalAgentResourcePolicy>(
    "get_global_agent_resource_policy",
  );
}

export async function setGlobalAgentResourcePolicy(
  policy: GlobalAgentResourcePolicy,
): Promise<GlobalAgentResourcePolicy> {
  return invokeTauri<GlobalAgentResourcePolicy>(
    "set_global_agent_resource_policy",
    { policy },
  );
}

export async function getDeviceMemorySnapshot(): Promise<DeviceMemorySnapshot> {
  return invokeTauri<DeviceMemorySnapshot>("get_device_memory_snapshot");
}
