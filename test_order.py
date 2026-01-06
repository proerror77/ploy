#!/usr/bin/env python3
"""Test order placement on Polymarket using official SDK approach"""

import requests
from py_clob_client.client import ClobClient
from py_clob_client.clob_types import OrderArgs, OrderType, ApiCreds, MarketOrderArgs
from py_clob_client.constants import POLYGON
from py_clob_client.order_builder.constants import BUY

# Configuration
# EOA private key (controls the proxy wallet)
PRIVATE_KEY = ""
# Proxy wallet address (holds the funds) - IMPORTANT for Magic/email wallets
FUNDER = "0xCbaAa60c5DEc85eaC2A2c424bdcD7258Ab67eEE2"
HOST = "https://clob.polymarket.com"
CHAIN_ID = POLYGON
# signature_type: 0 = EOA, 1 = Poly GNOSIS SAFE, 2 = Poly Proxy
SIGNATURE_TYPE = 2  # Poly Proxy for Magic wallets

def get_active_btc_market():
    """Get an active BTC UP/DOWN market from Gamma API"""
    try:
        # Get BTC series events
        resp = requests.get(
            "https://gamma-api.polymarket.com/events",
            params={"slug_contains": "btc-updown", "active": "true", "limit": 5},
            timeout=10
        )
        events = resp.json()

        for event in events:
            markets = event.get('markets', [])
            for m in markets:
                if 'Up' in m.get('question', '') or 'up' in m.get('outcome', '').lower():
                    clob_ids = m.get('clobTokenIds')
                    if clob_ids:
                        # clobTokenIds is [YES_token, NO_token] format
                        return {
                            'condition_id': m.get('conditionId'),
                            'up_token': clob_ids[0] if len(clob_ids) > 0 else None,
                            'down_token': clob_ids[1] if len(clob_ids) > 1 else None,
                            'question': m.get('question'),
                        }
    except Exception as e:
        print(f"Error fetching markets: {e}")

    return None


def main():
    print("=== Polymarket Order Test ===\n")
    print(f"Private Key: {PRIVATE_KEY[:20]}...")
    print(f"Funder (Proxy Wallet): {FUNDER}")
    print(f"Signature Type: {SIGNATURE_TYPE} (Poly Proxy)")

    # Initialize client with funder for proxy wallet
    print("\nInitializing client...")
    client = ClobClient(
        HOST,
        key=PRIVATE_KEY,
        chain_id=CHAIN_ID,
        signature_type=SIGNATURE_TYPE,
        funder=FUNDER
    )

    # Derive or create API credentials
    print("\nSetting up API credentials...")
    try:
        creds = client.create_or_derive_api_creds()
        print(f"✓ API credentials ready: {creds.api_key[:16]}...")
        client.set_api_creds(creds)
    except Exception as e:
        print(f"✗ Failed to get API credentials: {e}")
        # Fallback: try derive then create
        try:
            creds = client.derive_api_key()
            print(f"✓ Derived API Key: {creds.api_key[:16]}...")
            client.set_api_creds(creds)
        except:
            try:
                creds = client.create_api_key()
                print(f"✓ Created API Key: {creds.api_key[:16]}...")
                client.set_api_creds(creds)
            except Exception as e2:
                print(f"✗ All credential methods failed: {e2}")
                return

    # Get active market
    print("\nFetching active BTC market...")
    market = get_active_btc_market()

    if not market:
        print("✗ No active BTC market found via Gamma")
        # Try direct CLOB API with sampling
        print("\nTrying CLOB sampling endpoint...")
        try:
            # Get sampling markets which include crypto
            resp = requests.get(f"{HOST}/sampling-markets", timeout=10)
            data = resp.json()
            markets_list = data if isinstance(data, list) else data.get('data', [])
            for m in markets_list:
                q = m.get('question', '')
                if 'Bitcoin' in q and ('Up' in q or 'up' in q):
                    tokens = m.get('tokens', [])
                    if tokens:
                        market = {
                            'condition_id': m.get('condition_id'),
                            'up_token': tokens[0].get('token_id'),
                            'question': q,
                        }
                        print(f"Found via sampling: {q[:50]}...")
                        break
        except Exception as e:
            print(f"CLOB sampling failed: {e}")

        # Also try simplified-markets endpoint
        if not market:
            try:
                resp = requests.get(f"{HOST}/simplified-markets", timeout=10)
                data = resp.json()
                for m in data[:50]:
                    q = m.get('question', '')
                    if 'Bitcoin' in q and 'Up' in q:
                        tokens = m.get('tokens', [])
                        if tokens:
                            market = {
                                'condition_id': m.get('condition_id'),
                                'up_token': tokens[0].get('token_id'),
                                'question': q,
                            }
                            break
            except Exception as e:
                print(f"Simplified markets failed: {e}")

    if not market:
        print("✗ Could not find active market via API, using hardcoded token...")
        # Hardcode a known active BTC UP/DOWN token from EC2 query
        # These are from series 10192 (BTC Up/Down 15m)
        market = {
            'condition_id': '0x2abb7d676e4e236e730fa6f2730783d2552f5f1faea821791e4d47ff57b17245',
            'up_token': '64619299451087080124',  # BTC UP
            'down_token': '86343853725289728409',  # BTC DOWN
            'question': 'Bitcoin Up or Down - January 5, 2:15PM-2:30PM ET',
        }
        print(f"Using hardcoded market: {market['question']}")

    print(f"✓ Found market: {market.get('question', 'Unknown')[:60]}...")
    print(f"  Condition: {market.get('condition_id', 'N/A')[:20]}...")
    print(f"  UP Token: {market.get('up_token', 'N/A')}")

    TOKEN_ID = market.get('up_token')
    if not TOKEN_ID:
        print("✗ No token ID found")
        return

    # Check order book
    print(f"\nChecking order book...")
    try:
        book = client.get_order_book(TOKEN_ID)
        print(f"  Bids: {len(book.bids) if book.bids else 0}")
        print(f"  Asks: {len(book.asks) if book.asks else 0}")
        if book.bids and len(book.bids) > 0:
            print(f"  Best bid: {book.bids[0].price} x {book.bids[0].size}")
        if book.asks and len(book.asks) > 0:
            print(f"  Best ask: {book.asks[0].price} x {book.asks[0].size}")
    except Exception as e:
        print(f"✗ Order book failed: {e}")

    # Place test order
    print("\n=== Placing Test Order ===")
    print(f"Token: {TOKEN_ID}")
    print("Side: BUY")
    print("Size: 5")
    print("Price: 0.50")

    try:
        order_args = OrderArgs(
            token_id=TOKEN_ID,
            price=0.50,
            size=5,
            side="BUY",
        )

        signed_order = client.create_order(order_args)
        print(f"✓ Order signed")

        resp = client.post_order(signed_order, OrderType.GTC)
        print(f"\nOrder response: {resp}")

        if resp:
            order_id = getattr(resp, 'orderID', None) or resp.get('orderID') if isinstance(resp, dict) else None
            if order_id:
                print(f"\n✓ ORDER PLACED SUCCESSFULLY!")
                print(f"Order ID: {order_id}")

                # Cancel test order
                print("\nCancelling test order...")
                try:
                    cancel_resp = client.cancel(order_id)
                    print(f"✓ Order cancelled: {cancel_resp}")
                except Exception as ce:
                    print(f"Cancel failed: {ce}")
            else:
                print(f"Response: {resp}")

    except Exception as e:
        print(f"\n✗ Order failed: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    main()
