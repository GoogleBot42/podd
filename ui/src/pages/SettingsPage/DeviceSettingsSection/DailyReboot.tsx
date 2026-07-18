import { FormControlLabel, Switch, Typography } from '@mui/material';
import { DeepPartial } from 'ts-essentials';

import { Settings } from '@api/settingsSchema.ts';
import { useAppStore } from '@state/appStore.tsx';

type DailyRebootProps = {
  settings?: Settings;
  updateSettings: (settings: DeepPartial<Settings>) => void;
}

export default function DailyReboot({ settings, updateSettings }: DailyRebootProps) {
  const { isUpdating } = useAppStore();
  return (
    <>
      <FormControlLabel
        control={
          <Switch
            disabled={ isUpdating }
            checked={ settings?.rebootDaily || false }
            onChange={ (event) => updateSettings({ rebootDaily: event.target.checked }) }
          />
        }
        label="Reboot once a day"
      />
      <Typography color='text.secondary'>
        Automatically reboot the Pod once per day to keep it running smoothly.
        Reboot time is scheduled 1 hour before the daily prime time.
      </Typography>
    </>
  );
}
