Features:

    Free & Blazing Fast response
    No Rate limits
    200+ Currencies, Including Common Cryptocurrencies & Metals
    Daily Updated

URL Structure:

https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@{date}/{apiVersion}/{endpoint}
Formats:

date

The date should either be latest or in YYYY-MM-DD format

The Endpoints Supports HTTP GET Method and returns the data in two formats:

/{endpoint}.json

/{endpoint}.min.json
Endpoints:

    /currencies

    Lists all the available currencies in prettified json format:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies.json

    Get a minified version of it:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies.min.json

    /currencies/{currencyCode}

    Get the currency list with EUR as base currency:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/eur.json

    Get the currency list with EUR as base currency on date 2024-03-06:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@2024-03-06/v1/currencies/eur.json

    Get the currency list with BTC as base currency:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/btc.json

    Get the currency list with BTC as base currency in minified format:
    https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/btc.min.json

Additional Fallback URL on Cloudflare:

https://{date}.currency-api.pages.dev/{apiVersion}/{endpoint}

    Get the currency list with EUR as base currency:
    https://latest.currency-api.pages.dev/v1/currencies/eur.json

    Get the currency list with EUR as base currency on date 2024-03-06:
    https://2024-03-06.currency-api.pages.dev/v1/currencies/eur.json

Warning: Please include Fallback mechanism in your code, for example if cdn.jsdelivr.net link fails, fetch from currency-api.pages.dev

https://github.com/fawazahmed0/exchange-api?tab=readme-ov-file
