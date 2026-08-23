import _ from 'lodash';
import { create } from 'zustand';
import { DeepPartial } from 'ts-essentials';
import { DeviceStatus } from '@api/deviceStatusSchema.ts';


type ControlTempStore = {
  deviceStatus: DeviceStatus | undefined;
  setDeviceStatus: (newDeviceStatus: DeepPartial<DeviceStatus>) => void;
  // Count of optimistic temperature edits not yet confirmed by the server
  // (debounce pending or POST + confirm-refetch in flight). While nonzero,
  // refetched server state must not be synced into deviceStatus — it would
  // clobber the user's newer, still-unsent value.
  pendingEdits: number;
  beginEdit: () => void;
  endEdit: () => void;
  // Set when a temperature POST fails; the page surfaces it as a snackbar.
  // Happy-path updates are silent — no spinner, no disabling.
  updateError: boolean;
  setUpdateError: (updateError: boolean) => void;
  // The debounced temperature POST still waiting to fire, if any. Hoisted here
  // so it outlives TemperatureButtons (which unmounts the moment isOn goes
  // false) and so PowerButton can cancel it: the backend maps any setpoint
  // POST to enabled:true, so a stale edit flushed during a power-off would
  // turn the side back on (#105). flush() posts now; cancel() drops the edit
  // without posting (and balances the beginEdit).
  pendingTempPost: { flush: () => void; cancel: () => void } | null;
  setPendingTempPost: (pendingTempPost: { flush: () => void; cancel: () => void } | null) => void;
};

export const useControlTempStore = create<ControlTempStore>((set, get) => ({
  deviceStatus: undefined,
  setDeviceStatus: (newDeviceStatus) => {
    const { deviceStatus } = get();
    // Merge into a fresh object: mutating the stored one keeps the same
    // reference, so subscribers selecting state.deviceStatus never re-render.
    const updatedDeviceStatus = _.merge({}, deviceStatus, newDeviceStatus);
    set({ deviceStatus: updatedDeviceStatus });
  },
  pendingEdits: 0,
  beginEdit: () => set({ pendingEdits: get().pendingEdits + 1 }),
  endEdit: () => set({ pendingEdits: Math.max(0, get().pendingEdits - 1) }),
  updateError: false,
  setUpdateError: (updateError) => set({ updateError }),
  pendingTempPost: null,
  setPendingTempPost: (pendingTempPost) => set({ pendingTempPost }),
}));
