import { localHost, localHosts, type HostView } from '../ipc';

/** Discovery can fail without taking away the native local terminal. */
export async function loadLocalTargets(): Promise<HostView[]> {
  try { return await localHosts(); }
  catch { return [await localHost()]; }
}
