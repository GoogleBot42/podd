import { Dispatch, SetStateAction, useState } from 'react';
import {
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  Typography,
  useMediaQuery,
  useTheme,
} from '@mui/material';
import { overrideExpiry } from './alarmOverrideTarget.ts';
import { postSettings, useSettings } from '@api/settings.ts';
import { useAppStore } from '@state/appStore.tsx';


export interface AlarmDisabledDialogProps {
  open: boolean;
  setOpen: Dispatch<SetStateAction<boolean>>;
  scheduledAlarmTimeHhMm: string;
  alarmDisabled: boolean;
}

export default function AlarmDisabledDialog({
  open,
  setOpen,
  scheduledAlarmTimeHhMm,
  alarmDisabled,
}: AlarmDisabledDialogProps) {
  const theme = useTheme();
  const isSmallScreen = useMediaQuery(theme.breakpoints.down('sm'));
  const [isSaving, setIsSaving] = useState(false);
  const { data: settings, refetch } = useSettings();
  const side = useAppStore(state => state.side);

  const handleSave = () => {
    if (!settings) return null;
    // Expiry two minutes past the alarm's next occurrence — the daemon skips
    // every alarm starting before this, i.e. exactly the next one.
    const expiresAt = overrideExpiry(scheduledAlarmTimeHhMm, settings.timeZone);
    setIsSaving(true);
    const disabled = !alarmDisabled;
    postSettings({
      [side]: {
        scheduleOverrides: {
          alarm: {
            disabled: disabled,
            timeOverride: '',
            expiresAt: disabled ? expiresAt : '',
          }
        }
      }
    })
      .then(() => {
        setOpen(false);
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
    setOpen(false);
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
            height: '150px',
          },
      } }
    >
      <Typography variant="h5" textAlign="center">
        { alarmDisabled ? 'Enable' : 'Disable' } alarm for tonight?
      </Typography>

      <DialogActions
        sx={ {
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
        } }
      >

        { isSaving ? (
          <CircularProgress size={ 10 }/>
        ) : (
          <Box display="flex" gap={ 1 }>
            <Button variant="contained" color="error" size="small" onClick={ handleCancel }>
              Cancel
            </Button>
            <Button
              variant="contained"
              size="small"
              onClick={ handleSave }
            >
              Yes
            </Button>
          </Box>
        ) }
      </DialogActions>
    </Dialog>
  );
}
