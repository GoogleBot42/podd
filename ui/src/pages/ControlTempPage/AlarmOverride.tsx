import { Dispatch, SetStateAction, useState } from 'react';
import {
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  InputAdornment,
  TextField,
  Typography,
  useMediaQuery,
  useTheme,
} from '@mui/material';
import AccessTime from '@mui/icons-material/AccessTime';
import { overrideExpiry } from './alarmOverrideTarget.ts';
import { postSettings, useSettings } from '@api/settings.ts';
import { useAppStore } from '@state/appStore.tsx';

export interface AlarmOverrideProps {
  open: boolean;
  alarmTimeLocalOverride: string;
  scheduledAlarmTimeHhMm: string;

  setAlarmTimeLocalOverride: Dispatch<SetStateAction<string>>;
  setOverrideOpen: Dispatch<SetStateAction<boolean>>;
}

export default function AlarmOverride({
  open,
  setOverrideOpen,
  alarmTimeLocalOverride,
  scheduledAlarmTimeHhMm,
  setAlarmTimeLocalOverride,
}: AlarmOverrideProps) {
  const theme = useTheme();
  const isSmallScreen = useMediaQuery(theme.breakpoints.down('sm'));
  const [isSaving, setIsSaving] = useState(false);
  const { data: settings, refetch } = useSettings();
  const side = useAppStore(state => state.side);

  const handleSave = () => {
    if (!settings) return null;
    // Expiry two minutes past the moved alarm's next occurrence — the daemon
    // applies the override to the one alarm whose moved start precedes this.
    const expiresAt = overrideExpiry(alarmTimeLocalOverride, settings.timeZone);
    setIsSaving(true);

    postSettings({
      [side]: {
        scheduleOverrides: {
          alarm: {
            disabled: false,
            timeOverride: alarmTimeLocalOverride,
            expiresAt: expiresAt,
          }
        }
      }
    })
      .then(() => {
        setAlarmTimeLocalOverride('');
        setOverrideOpen(false);
        return refetch();
      })
      .catch(error => {
        console.error(error);
      })
      .finally(() => {
        setIsSaving(false);
      });
  };

  const handleCancel = () => {
    setAlarmTimeLocalOverride('');
    setOverrideOpen(false);
  };

  return (
    <Dialog
      open={ open }
      fullScreen={ isSmallScreen }
      PaperProps={ {
        sx: isSmallScreen
          ? {
            display: 'flex',
            justifyContent: 'center',
            alignItems: 'center',
            textAlign: 'center',
            maxWidth: '85vw',
            maxHeight: '35vh',
            borderRadius: '10px',
            margin: 0,
            p: 4,
          }
          : {
            p: 4,
            width: '50%',
            height: '225px',
          },
      } }
    >
      <Typography variant="h5" textAlign="center">
        Override alarm for tonight?
      </Typography>

      <DialogActions
        sx={ {
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
        } }
      >
        <TextField
          label="Alarm"
          type="time"
          value={ alarmTimeLocalOverride || scheduledAlarmTimeHhMm }
          onChange={ (e) => setAlarmTimeLocalOverride(e.target.value) }
          variant="standard"
          sx={ {
            width: '110px',
            '& input::-webkit-calendar-picker-indicator': {
              opacity: 0,
              display: 'none',
            },
          } }
          InputProps={ {
            endAdornment: (
              <InputAdornment position="end" sx={ { cursor: 'pointer' } }>
                <AccessTime sx={ { color: theme.palette.grey[500] } } fontSize="small" />
              </InputAdornment>
            ),
          } }
        />

        <br />

        { isSaving ? (
          <CircularProgress size={ 10 } />
        ) : (
          <Box display="flex" gap={ 1 }>
            <Button variant="contained" color="error" size="small" onClick={ handleCancel }>
              Cancel
            </Button>
            <Button
              variant="contained"
              size="small"
              onClick={ handleSave }
              disabled={ !alarmTimeLocalOverride || alarmTimeLocalOverride === scheduledAlarmTimeHhMm }
            >
              Save
            </Button>
          </Box>
        ) }
      </DialogActions>
    </Dialog>
  );
}
