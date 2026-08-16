import _ from 'lodash';
import { create } from 'zustand';
import { DeepPartial } from 'ts-essentials';
import { DeviceStatus } from '@api/deviceStatusSchema.ts';


type ControlTempStore = {
  deviceStatus: DeviceStatus | undefined;
  setDeviceStatus: (newDeviceStatus: DeepPartial<DeviceStatus>) => void;
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
}));
