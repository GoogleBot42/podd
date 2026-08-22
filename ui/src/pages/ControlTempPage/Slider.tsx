import { useRef } from 'react';
import { CircularSliderWithChildren } from 'react-circular-slider-svg';
import { postDeviceStatus } from '@api/deviceStatus.ts';
import { useAppStore } from '@state/appStore';
import styles from './Slider.module.scss';
import TemperatureLabel from './TemperatureLabel.tsx';
import TemperatureButtons from './TemperatureButtons.tsx';
import { useControlTempStore } from './controlTempStore.tsx';
import { useTheme } from '@mui/material/styles';
import { useResizeDetector } from 'react-resize-detector';
import { useSettings } from '@api/settings.ts';
import { MAX_TEMP_F, MIN_TEMP_F, getTemperatureColor } from '@lib/temperatureConversions.ts';

// How far (px) from the track's centerline a press still counts as grabbing the
// slider. The library itself accepts presses anywhere in its square SVG, which
// makes it far too easy to set a temperature by tapping near — but not on — the
// arc; anything outside this band is swallowed before the library sees it.
const TRACK_HIT_TOLERANCE_PX = 28;

type SliderProps = {
  isOn: boolean;
  currentTargetTemp: number;
  currentTemperatureF: number;
  refetch: any;
  displayCelsius: boolean;
}

export default function Slider({ isOn, currentTargetTemp, refetch, currentTemperatureF, displayCelsius }: SliderProps) {
  const { deviceStatus, setDeviceStatus } = useControlTempStore();
  const { isUpdating, setIsUpdating, side } = useAppStore();
  const { data: settings } = useSettings();
  const isInAwayMode = settings?.[side].awayMode;
  const disabled = isUpdating || isInAwayMode || !isOn;
  const { width, ref } = useResizeDetector();
  const theme = useTheme();
  const sliderColor = getTemperatureColor(deviceStatus?.[side]?.targetTemperatureF);
  const handleControlFinished = async () => {
    // Send-time read (see TemperatureButtons.postUpdate): this closure can be
    // one render behind the last onChange when the drag ends quickly.
    const current = useControlTempStore.getState().deviceStatus;
    const targetTemperatureF = current?.[side]?.targetTemperatureF;
    if (targetTemperatureF === undefined) return;

    useControlTempStore.getState().beginEdit();
    setIsUpdating(true);
    void postDeviceStatus({
      [side]: {
        targetTemperatureF
      }
    })
      .then(() => {
        // Wait 1 second before refreshing the device status
        return new Promise((resolve) => setTimeout(resolve, 1_500));
      })
      .then(() => refetch())
      .catch(error => {
        console.error(error);
      })
      .finally(() => {
        setIsUpdating(false);
        useControlTempStore.getState().endEdit();
      });
  };

  const arcBackgroundColor = theme.palette.grey[700];

  // True while the current touch interaction started on the track, so its
  // move/end events may reach the slider.
  const touchOnTrackRef = useRef(false);

  // The track's centerline radius mirrors react-circular-slider-svg's own
  // geometry: trackInnerRadius = size/2 - trackWidth - 20 (shadow width).
  const isNearTrack = (clientX: number, clientY: number, target: HTMLElement) => {
    const rect = target.getBoundingClientRect();
    const trackRadius = rect.width / 2 - 20 - 6 / 2;
    const distance = Math.hypot(
      clientX - (rect.left + rect.width / 2),
      clientY - (rect.top + rect.height / 2),
    );
    return Math.abs(distance - trackRadius) <= TRACK_HIT_TOLERANCE_PX;
  };

  const sideStatus = deviceStatus?.[side];
  const minTemp = Math.min(sideStatus?.currentTemperatureF || 55, sideStatus?.targetTemperatureF || 55);
  const maxTemp = Math.max(sideStatus?.currentTemperatureF || 55, sideStatus?.targetTemperatureF || 55);
  const isHeating = (sideStatus?.currentTemperatureF ?? 55) < (sideStatus?.targetTemperatureF ?? 55);

  return (
    <div
      ref={ ref }
      style={ { position: 'relative', display: 'inline-block', width: '100%', maxWidth: '400px' } }
    >
      { /* Circular Slider */ }
      <div
        className={ `${styles.Slider} ${disabled && styles.Disabled} ${isHeating && styles.Heating}` }
        onMouseDownCapture={ (e) => {
          if (!isNearTrack(e.clientX, e.clientY, e.currentTarget)) e.stopPropagation();
        } }
        onTouchStartCapture={ (e) => {
          const touch = e.touches[0];
          touchOnTrackRef.current = touch != null && isNearTrack(touch.clientX, touch.clientY, e.currentTarget);
          if (!touchOnTrackRef.current) e.stopPropagation();
        } }
        onTouchMoveCapture={ (e) => {
          if (!touchOnTrackRef.current) e.stopPropagation();
        } }
        onTouchEndCapture={ (e) => {
          if (!touchOnTrackRef.current) e.stopPropagation();
        } }
      >
        <CircularSliderWithChildren
          disabled={ disabled }
          onControlFinished={ handleControlFinished }
          size={ width }
          trackWidth={ 6 }
          minValue={ MIN_TEMP_F }
          maxValue={ MAX_TEMP_F }
          startAngle={ 60 }
          endAngle={ 300 }
          angleType={ {
            direction: 'cw',
            axis: '-y'
          } }
          handle1={ {
            value: minTemp,
            onChange: (value) => {
              if (disabled) return;
              if (Math.round(value) !== deviceStatus?.[side]?.targetTemperatureF) {
                setDeviceStatus({ [side]: { targetTemperatureF: Math.round(value) } });
              }
            },

          } }
          arcColor={ isOn ? sliderColor : arcBackgroundColor }
          arcBackgroundColor={ arcBackgroundColor }
          handle2={ {
            value: maxTemp,
            onChange: (value) => {
              if (disabled) return;
              if (Math.round(value) !== deviceStatus?.[side]?.targetTemperatureF) {
                setDeviceStatus({ [side]: { targetTemperatureF: Math.round(value) } });
              }
            },
          } }
          handleSize={ 8 }
        >
          <TemperatureLabel
            isOn={ isOn }
            sliderTemp={ deviceStatus?.[side]?.targetTemperatureF || 55 }
            sliderColor={ sliderColor }
            currentTargetTemp={ currentTargetTemp }
            currentTemperatureF={ currentTemperatureF }
            displayCelsius={ displayCelsius }
          />
        </CircularSliderWithChildren>
      </div>
      {
        isOn && (
          <TemperatureButtons refetch={ refetch } currentTargetTemp={ currentTargetTemp }/>
        ) }
    </div>
  );
};
