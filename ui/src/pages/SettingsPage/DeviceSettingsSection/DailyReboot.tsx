import { FormControlLabel, Switch, Typography } from '@mui/material';
import { DeepPartial } from 'ts-essentials';

import { Settings } from '@api/settingsSchema.ts';

type DailyRebootProps = {
  settings?: Settings;
  updateSettings: (settings: DeepPartial<Settings>) => void;
}

export default function DailyReboot({ settings, updateSettings }: DailyRebootProps) {
  return (
    <>
      <FormControlLabel
        control={
          <Switch
            // podd has no reboot scheduler at all, so the toggle would only
            // persist a setting nothing acts on.
            disabled
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
      <Typography variant='body2' color='text.secondary'>
        Not yet supported by podd
      </Typography>
    </>
  );
}
