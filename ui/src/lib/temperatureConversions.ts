export function farenheitToCelcius(farenheit: number): number {
  const celcius = (farenheit - 32) * 5 / 9;
  return Math.round(celcius * 2) / 2;
}

export function formatTemperature(temperature: number, celcius: boolean) {
  return celcius ? `${farenheitToCelcius(temperature)}°C` : `${temperature}°F`;
}


export const MIN_TEMP_F = 55;
export const MAX_TEMP_F = 110;

export const MIN_TEMP_C = farenheitToCelcius(MIN_TEMP_F);
export const MAX_TEMP_C = farenheitToCelcius(MAX_TEMP_F);


export function getTemperatureColor(tempF: number | undefined) {
  if (tempF === undefined) return '#262626';
  if (tempF <= 70) return '#2196f3';
  if (tempF <= 82) return '#5393ff';
  if (tempF <= 95) return '#db5858';
  return '#d32f2f';
}

