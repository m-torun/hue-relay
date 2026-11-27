# Disclaimer

**Use hue-relay at your own risk.** By using it you accept everything below. This is not legal
advice; if you need certainty about your situation, ask a qualified professional.

## No warranty, no liability

hue-relay is provided "as is", without warranty of any kind, express or implied, including fitness
for a particular purpose. The authors and contributors are not liable for any claim, damage or other
liability arising from its use, including damage to devices, loss of data, malfunctioning lights,
lost subscriptions, voided warranties, injury, or any consequence of the points below, to the extent
the law allows. See sections 15 and 16 of the [license](LICENSE).

## Not affiliated

hue-relay is an independent project. It is not affiliated with, endorsed, sponsored or supported by
Signify (Philips Hue), LG Electronics, or any other manufacturer or service provider. Philips Hue,
Hue Sync and Hue Entertainment are trademarks of Signify Holding; LG and webOS are trademarks of LG
Electronics; other names are trademarks of their owners. They are used only to describe what
hue-relay works with.

## Warranties, terms of service and support

hue-relay presents itself to Hue Sync clients as a Hue bridge and streams to your bridge with its
Entertainment API. Using it may go against the terms of service, licence terms or warranty
conditions of your devices, apps or subscriptions, and manufacturers may refuse support or warranty
claims for setups that involve it. Check those terms before you use it. Parts of it rely on
undocumented behaviour, which any firmware or app update can change or break without notice.

## Laws, regulations and radio use

You are responsible for complying with the laws, regulations and agreements that apply where you
live, including those on radio equipment, electrical installations and product modifications.

hue-relay itself does not transmit radio signals and does not change the radio settings, firmware or
hardware of any device: it talks to your Hue bridge over your local network, and the bridge and
lights use their own radios (Zigbee) as designed. If you modify hardware, radio equipment, antennas
or firmware, or operate equipment outside its approved conditions, that is your responsibility,
including compliance with radio and product-safety rules (for example FCC rules in the United
States, or the Radio Equipment Directive and CE marking in the European Union).

## Safety

hue-relay changes how your lights behave. A wrong configuration, a bug, or a crash can leave lights
off, on, dimmer or brighter than intended. Do not use it for lighting that safety depends on
(emergency, stair or medical lighting). Light sync can show fast-changing colours and brightness,
which can affect people with photosensitive epilepsy or migraines.

## Network security

hue-relay adds a device to your network that answers like a Hue bridge and holds a key to your real
bridge. While pairing is open (`link_open`), anyone on your network can pair with it and control
the lights it relays. Run it on networks you trust, close pairing once your clients are paired, and
never expose it to the internet.
