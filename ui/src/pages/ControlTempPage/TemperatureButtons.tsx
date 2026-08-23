import { useRef, useCallback, useEffect } from 'react';
import { useTheme } from '@mui/material/styles';
import { Button, Box } from '@mui/material';
import { Add, Remove } from '@mui/icons-material';
import { useControlTempStore } from './controlTempStore.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { postDeviceStatus } from '@api/deviceStatus.ts';
import { useSettings } from '@api/settings.ts';
import { MIN_TEMP_F, MAX_TEMP_F } from '@lib/temperatureConversions.ts';

type TemperatureButtonsProps = {
  refetch: any;
  currentTargetTemp: number;
}

const DEBOUNCE_MS = 2000;
export default function TemperatureButtons({ refetch, currentTargetTemp }: TemperatureButtonsProps) {
  const { side, isUpdating } = useAppStore();
  const setDeviceStatus = useControlTempStore(state => state.setDeviceStatus);
  const optimisticTemp = useControlTempStore(state => state.deviceStatus?.[side]?.targetTemperatureF);
  const { data: settings } = useSettings();
  const theme = useTheme();
  const debounceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const postUpdate = useCallback(async () => {
    // Read the target at send time, not from this render's closure: the
    // debounce timer outlives renders, and a closure snapshot posts the value
    // as of the *previous* press (one step short).
    const current = useControlTempStore.getState().deviceStatus;
    try {
      await postDeviceStatus({
        [side]: { targetTemperatureF: current?.[side]?.targetTemperatureF },
      });
      await new Promise(r => setTimeout(r, 1_500));
      await refetch?.();
    } catch (err) {
      // Happy path is silent; on failure surface the snackbar and refetch so
      // the display falls back to the server's actual state.
      console.error(err);
      useControlTempStore.getState().setUpdateError(true);
      await refetch?.();
    } finally {
      useControlTempStore.getState().endEdit();
    }
  }, [side, refetch]);

  const scheduleUpdate = useCallback(() => {
    if (debounceTimer.current) {
      clearTimeout(debounceTimer.current);
    } else {
      // Start of a new debounce chain; the matching endEdit runs in
      // postUpdate's finally once this chain's single post completes (or in
      // cancel() if the chain is dropped).
      useControlTempStore.getState().beginEdit();
    }
    // The pending post lives in the store (not a local ref) so PowerButton can
    // cancel it before posting isOn:false — see pendingTempPost in the store.
    const flush = () => {
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
      debounceTimer.current = null;
      useControlTempStore.getState().setPendingTempPost(null);
      void postUpdate();
    };
    const cancel = () => {
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
      debounceTimer.current = null;
      useControlTempStore.getState().setPendingTempPost(null);
      useControlTempStore.getState().endEdit();
    };
    useControlTempStore.getState().setPendingTempPost({ flush, cancel });
    debounceTimer.current = setTimeout(flush, DEBOUNCE_MS);
  }, [postUpdate]);

  useEffect(() => {
    const flush = () => useControlTempStore.getState().pendingTempPost?.flush();
    const onVisibilityChange = () => {
      if (document.visibilityState === 'hidden') flush();
    };
    window.addEventListener('pagehide', flush);
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => {
      window.removeEventListener('pagehide', flush);
      document.removeEventListener('visibilitychange', onVisibilityChange);
      // Unmount usually means navigation — flush so the edit isn't lost. But
      // when the side just went off (power button or schedule), this very
      // unmount is caused by the power-off, and flushing would post a setpoint
      // that re-enables the side (#105) — drop the edit instead.
      const pending = useControlTempStore.getState().pendingTempPost;
      if (!pending) return;
      const stillOn = useControlTempStore.getState().deviceStatus?.[useAppStore.getState().side]?.isOn;
      if (stillOn === false) pending.cancel();
      else pending.flush();
    };
  }, []);


  const isInAwayMode = settings?.[side].awayMode;
  if (isInAwayMode) return null;

  const disabled = isUpdating || isInAwayMode;
  // Bound the buttons on the displayed (optimistic) value, not the server's —
  // presses stay enabled while a post is in flight, so the prop lags.
  const displayTemp = optimisticTemp ?? currentTargetTemp;
  const borderColor = theme.palette.grey[800];
  const iconColor = theme.palette.grey[500];

  const handleClick = (change: number) => {
    // Same send-time read as postUpdate: two presses inside one render batch
    // must stack, not both compute from the same snapshot.
    const current = useControlTempStore.getState().deviceStatus;
    const currentTemp = current?.[side]?.targetTemperatureF;
    if (currentTemp === undefined) return;
    setDeviceStatus({
      [side]: {
        targetTemperatureF: Math.min(MAX_TEMP_F, Math.max(MIN_TEMP_F, currentTemp + change)),
      }
    });

    scheduleUpdate();
  };

  const buttonStyle = {
    borderWidth: '2px',
    borderColor,
    width: 50,
    height: 50,
    borderRadius: '50%',
    minWidth: 0,
    padding: 0,
  };

  return (
    <Box
      sx={ {
        top: '75%',
        position: 'absolute',
        display: 'flex',
        justifyContent: 'center',
        alignItems: 'center',
        gap: '100px',
        width: '100%',
        marginLeft: 'auto',
        marginRight: 'auto',
      } }
    >
      <Button
        variant="outlined"
        color="primary"
        sx={ buttonStyle }
        onClick={ () => handleClick(-1) }
        disabled={ disabled || displayTemp <= MIN_TEMP_F }
      >
        <Remove sx={ { color: iconColor } }/>
      </Button>
      <Button
        variant="outlined"
        sx={ buttonStyle }

        onClick={ () => handleClick(1) }
        disabled={ disabled || displayTemp >= MAX_TEMP_F }
      >
        <Add sx={ { color: iconColor } }/>
      </Button>
    </Box>
  );
}
