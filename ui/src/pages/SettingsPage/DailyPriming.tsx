import { Box, FormControlLabel, InputAdornment, TextField } from '@mui/material';
import { useTheme } from '@mui/material/styles';

import { Settings } from '@api/settingsSchema.ts';
import { DeepPartial } from 'ts-essentials';
import { useAppStore } from '@state/appStore.tsx';
import Switch from '@mui/material/Switch';
import AccessTime from '@mui/icons-material/AccessTime';


type PrimePodScheduleProps = {
  settings?: Settings;
  updateSettings: (settings: DeepPartial<Settings>) => void;
}

export default function DailyPriming({ settings, updateSettings }: PrimePodScheduleProps) {
  const { isUpdating } = useAppStore();
  const theme = useTheme();

  return (
    <>
      <Box sx={ { display: 'flex', alignItems: 'center', gap: 2, mb: 1 } }>
        <FormControlLabel
          control={
            <Switch
              disabled={ isUpdating }
              checked={ settings?.primePodDaily?.enabled || false }
              onChange={ (event) => updateSettings({ primePodDaily: { enabled: event.target.checked } }) }
            />
          }
          label="Prime daily?"
        />
        <TextField
          label="Prime time"
          type="time"
          size='medium'
          variant='standard'
          value={ settings?.primePodDaily?.time || '12:00' }
          onChange={ (e) => updateSettings({ primePodDaily: { time: e.target.value } }) }
          disabled={ isUpdating || settings?.primePodDaily?.enabled === false }
          sx={ {
            width: '110px',
            // Hide native indicator (where it exists)
            '& input::-webkit-calendar-picker-indicator': {
              opacity: 0,
              display: 'none',
            },
          } }
          InputProps={ {
            endAdornment: (
              <InputAdornment position="end" sx={ { cursor: 'pointer' } } >
                <AccessTime sx={ { color: theme.palette.grey[500] } } fontSize='small'/>
              </InputAdornment>
            ),
          } }
        />
      </Box>
    </>

  );
}
