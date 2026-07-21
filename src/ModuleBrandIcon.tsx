import airbnbLogo from "./assets/logos/airbnb.svg";
import doordashLogo from "./assets/logos/doordash.svg";
import grokLogo from "./assets/logos/grok.svg";
import instagramLogo from "./assets/logos/instagram.svg";
import kickLogo from "./assets/logos/kick.svg";
import maxLogo from "./assets/logos/max.svg";
import openaiLogo from "./assets/logos/openai.svg";
import redditLogo from "./assets/logos/reddit.svg";
import sephoraLogo from "./assets/logos/sephora.png";
import spotifyLogo from "./assets/logos/spotify.svg";
import stockxLogo from "./assets/logos/stockx.svg";
import tiktokLogo from "./assets/logos/tiktok.svg";
import twitchLogo from "./assets/logos/twitch.svg";
import uberLogo from "./assets/logos/uber.svg";
import zaiLogo from "./assets/logos/zai.svg";

const brandLogos: Record<string, string> = {
  airbnb: airbnbLogo,
  chatgpt: openaiLogo,
  doordash: doordashLogo,
  grok: grokLogo,
  instagram: instagramLogo,
  kick: kickLogo,
  max: maxLogo,
  reddit: redditLogo,
  sephora: sephoraLogo,
  spotify: spotifyLogo,
  stockx: stockxLogo,
  tiktok: tiktokLogo,
  twitch: twitchLogo,
  uber: uberLogo,
  zai: zaiLogo,
};

export function ModuleBrandIcon({ id }: { id: string }) {
  const logo = brandLogos[id];

  if (logo) {
    return <img className="module-brand-img" src={logo} alt="" aria-hidden="true" draggable={false} />;
  }

  return <span className="module-brand-fallback" aria-hidden="true">{id.slice(0, 1).toUpperCase()}</span>;
}
