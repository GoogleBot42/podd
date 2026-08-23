import { useEffect, useState } from 'react';
import {
  Alert,
  Dialog,
  DialogActions,
  Button,
  Snackbar,
  useMediaQuery,
  useTheme,
} from '@mui/material';
import { Side, useAppStore } from '@state/appStore.tsx';
import AlarmIcon from '@mui/icons-material/Alarm';
import { keyframes } from '@mui/system';
import { postDeviceStatus } from '@api/deviceStatus.ts';
import { useControlTempStore } from './controlTempStore.tsx';


type AlarmDismissalProps = {
  refetch: any;
}

const pulse = keyframes`
    0% { transform: scale(1) translateX(0); }
    10% { transform: scale(1.1) translateX(-3px); }
    20% { transform: scale(1.1) translateX(3px); }
    30% { transform: scale(1.1) translateX(-3px); }
    40% { transform: scale(1.1) translateX(3px); }
    50% { transform: scale(1.2) translateX(0); }
    100% { transform: scale(1) translateX(0); }
`;


export default function AlarmDismissal({ refetch }: AlarmDismissalProps) {
  const { side, setIsUpdating } = useAppStore();
  const deviceStatus = useControlTempStore(state => state.deviceStatus);

  // Latched per side: dismissing the left alarm must not suppress the right
  // side's dialog. The latch only hides a dialog for an alarm that is still
  // vibrating, so it clears itself once the alarm stops and a second alarm in
  // the same session shows the dialog again.
  const [dismissed, setDismissed] = useState<Record<Side, boolean>>({ left: false, right: false });
  const [dismissError, setDismissError] = useState(false);

  const theme = useTheme();
  const isSmallScreen = useMediaQuery(theme.breakpoints.down('sm'));

  const leftVibrating = deviceStatus?.left?.isAlarmVibrating || false;
  const rightVibrating = deviceStatus?.right?.isAlarmVibrating || false;

  useEffect(() => {
    setDismissed(previous => ({
      left: previous.left && leftVibrating,
      right: previous.right && rightVibrating,
    }));
  }, [leftVibrating, rightVibrating]);

  const handleDismiss = () => {
    setIsUpdating(true);
    const dismissedSide = side;
    postDeviceStatus({
      [dismissedSide]: {
        isAlarmVibrating: false,
      }
    })
      .then(() => {
        // Only a confirmed dismiss may hide the dialog — it is the one control
        // that stops a bed that is physically vibrating.
        setDismissed(previous => ({ ...previous, [dismissedSide]: true }));
        // Wait 1 second before refreshing the device status
        return new Promise((resolve) => setTimeout(resolve, 1_000));
      })
      .then(() => refetch())
      .catch(error => {
        console.error(error);
        setDismissError(true);
      })
      .finally(() => {
        setIsUpdating(false);
      });
  };

  return (
    <>
      <Dialog
        open={ !dismissed[side] && (deviceStatus?.[side]?.isAlarmVibrating || false) }
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
            }
            : {
              width: '50%',
              height: '200px'
            },
        } }
      >
        <DialogActions
          sx={ {
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            justifyContent: 'center',
            height: '100%',
          } }
        >
          <AlarmIcon fontSize="large" sx={ { mb: 4,animation: `${pulse} 2s infinite`, } }/>
          <Button
            onClick={ handleDismiss }
            color="error"
            variant="contained"
          >
            Dismiss Alarm
          </Button>
        </DialogActions>
      </Dialog>
      <Snackbar
        open={ dismissError }
        autoHideDuration={ 6000 }
        onClose={ () => setDismissError(false) }
      >
        <Alert severity="error" onClose={ () => setDismissError(false) }>
          Couldn&apos;t dismiss the alarm — try again
        </Alert>
      </Snackbar>
    </>
  );
}
