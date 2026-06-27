"""
Notification Preferences Service Client
Provides access to defaults and user-specific notification preferences.
"""

import logging
from dataclasses import dataclass
from enum import Enum
from typing import Any, Dict, List, Optional, Union
from urllib.parse import urljoin

import aiohttp
import requests
from aiohttp import ClientSession, ClientTimeout
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry

logger = logging.getLogger(__name__)


class Channel(str, Enum):
    """Notification delivery channels."""
    EMAIL = "email"
    PUSH = "push"
    SMS = "sms"
    IN_APP = "in_app"


class APIError(Exception):
    """Base exception for API errors."""
    
    def __init__(self, message: str, status_code: Optional[int] = None, response: Optional[Any] = None):
        super().__init__(message)
        self.status_code = status_code
        self.response = response


class NotFoundError(APIError):
    """Resource not found error."""
    pass


class ServerError(APIError):
    """Internal server error."""
    pass


class ValidationError(APIError):
    """Request validation error."""
    pass


@dataclass
class PreferenceResponse:
    """Response from preference GET operations."""
    user: str
    subject: str
    channel: Channel
    is_default: bool = False


@dataclass
class BatchPreferenceRequest:
    """Batch preference operation request."""
    user: str
    subject: str
    channel: Channel


class RetryConfig:
    """Configuration for retry behavior."""
    
    def __init__(
        self,
        total_retries: int = 3,
        backoff_factor: float = 0.5,
        status_forcelist: tuple = (429, 500, 502, 503, 504),
        allowed_methods: tuple = ("GET", "POST", "PUT", "DELETE", "PATCH")
    ):
        self.total_retries = total_retries
        self.backoff_factor = backoff_factor
        self.status_forcelist = status_forcelist
        self.allowed_methods = allowed_methods


class NotificationPrefsClient:
    """
    Client for the Notification Preferences Service.
    
    Provides methods to manage default notification channels and user-specific
    notification preferences with both synchronous and asynchronous interfaces.
    
    Examples:
        # Synchronous usage
        client = NotificationPrefsClient("http://localhost:8080")
        client.set_default("marketing", Channel.EMAIL)
        channel = client.get_preference("user123", "marketing")
        
        # Asynchronous usage
        async with NotificationPrefsClient("http://localhost:8080") as client:
            await client.async_set_preference("user123", "marketing", Channel.PUSH)
            channel = await client.async_get_preference("user123", "marketing")
    """
    
    def __init__(
        self,
        base_url: str,
        api_version: str = "v1",
        timeout: float = 30.0,
        retry_config: Optional[RetryConfig] = None,
        headers: Optional[Dict[str, str]] = None,
        verify_ssl: bool = True
    ):
        """
        Initialize the notification preferences client.
        
        Args:
            base_url: Base URL of the notification service
            api_version: API version prefix (use None for no prefix)
            timeout: Request timeout in seconds
            retry_config: Configuration for request retries
            headers: Additional HTTP headers
            verify_ssl: Whether to verify SSL certificates
        """
        self.base_url = base_url.rstrip("/")
        self.api_version = api_version
        self.timeout = timeout
        self.verify_ssl = verify_ssl
        self.default_headers = headers or {}
        
        # Build API prefix
        if api_version:
            self.api_prefix = f"/{api_version}"
        else:
            self.api_prefix = ""
        
        # Configure retries
        self.retry_config = retry_config or RetryConfig()
        
        # Initialize synchronous session
        self._sync_session: Optional[requests.Session] = None
        self._initialize_sync_session()
        
        # Placeholder for async session
        self._async_session: Optional[ClientSession] = None
    
    def _initialize_sync_session(self):
        """Initialize the synchronous requests session with retry logic."""
        self._sync_session = requests.Session()
        
        # Configure retry strategy
        retry_strategy = Retry(
            total=self.retry_config.total_retries,
            backoff_factor=self.retry_config.backoff_factor,
            status_forcelist=self.retry_config.status_forcelist,
            allowed_methods=self.retry_config.allowed_methods
        )
        
        adapter = HTTPAdapter(max_retries=retry_strategy)
        self._sync_session.mount("http://", adapter)
        self._sync_session.mount("https://", adapter)
        self._sync_session.verify = self.verify_ssl
        self._sync_session.headers.update(self.default_headers)
    
    def _build_url(self, endpoint: str) -> str:
        """Build the full URL for an API endpoint."""
        return urljoin(f"{self.base_url}{self.api_prefix}", endpoint.lstrip("/"))
    
    def _handle_response(self, response: requests.Response) -> Any:
        """Handle synchronous API response."""
        if response.status_code == 200:
            return response.json() if response.text else None
        elif response.status_code == 404:
            raise NotFoundError(
                f"Resource not found: {response.url}",
                status_code=response.status_code,
                response=response.text
            )
        elif response.status_code >= 500:
            raise ServerError(
                f"Server error: {response.text}",
                status_code=response.status_code,
                response=response.text
            )
        elif response.status_code >= 400:
            raise ValidationError(
                f"Client error: {response.text}",
                status_code=response.status_code,
                response=response.text
            )
        else:
            raise APIError(
                f"Unexpected status code: {response.status_code}",
                status_code=response.status_code,
                response=response.text
            )
    
    async def _async_handle_response(self, response: aiohttp.ClientResponse) -> Any:
        """Handle asynchronous API response."""
        if response.status == 200:
            text = await response.text()
            return await response.json() if text else None
        elif response.status == 404:
            text = await response.text()
            raise NotFoundError(
                f"Resource not found: {response.url}",
                status_code=response.status,
                response=text
            )
        elif response.status >= 500:
            text = await response.text()
            raise ServerError(
                f"Server error: {text}",
                status_code=response.status,
                response=text
            )
        elif response.status >= 400:
            text = await response.text()
            raise ValidationError(
                f"Client error: {text}",
                status_code=response.status,
                response=text
            )
        else:
            text = await response.text()
            raise APIError(
                f"Unexpected status code: {response.status}",
                status_code=response.status,
                response=text
            )
    
    # =========================================================================
    # Synchronous Methods
    # =========================================================================
    
    def set_default(self, subject: str, channel: Channel) -> None:
        """
        Set a default notification channel for a subject.
        
        Args:
            subject: The notification subject type (e.g., "marketing", "security")
            channel: The default channel to use
            
        Raises:
            APIError: If the API request fails
            ValidationError: If the request is invalid
            
        Example:
            client.set_default("marketing", Channel.EMAIL)
        """
        url = self._build_url("/defaults/set")
        payload = {
            "subject": subject,
            "channel": channel.value
        }
        
        response = self._sync_session.post(
            url,
            json=payload,
            timeout=self.timeout
        )
        
        self._handle_response(response)
        logger.info(f"Set default channel for subject '{subject}' to {channel.value}")
    
    def get_default(self, subject: str) -> Channel:
        """
        Get the default notification channel for a subject.
        
        Args:
            subject: The notification subject type
            
        Returns:
            Channel: The default channel for the subject
            
        Raises:
            NotFoundError: If no default exists for the subject
            APIError: If the API request fails
            
        Example:
            channel = client.get_default("marketing")
        """
        url = self._build_url("/defaults/get")
        params = {"subject": subject}
        
        response = self._sync_session.get(
            url,
            params=params,
            timeout=self.timeout
        )
        
        data = self._handle_response(response)
        return Channel(data) if isinstance(data, str) else Channel(data["channel"])
    
    def get_default_or_none(self, subject: str) -> Optional[Channel]:
        """
        Get the default channel for a subject, returning None if not found.
        
        Args:
            subject: The notification subject type
            
        Returns:
            Optional[Channel]: The default channel, or None if not found
            
        Example:
            channel = client.get_default_or_none("marketing")
        """
        try:
            return self.get_default(subject)
        except NotFoundError:
            return None
    
    def set_preference(self, user: str, subject: str, channel: Channel) -> None:
        """
        Set a user-specific notification preference.
        
        Args:
            user: The user identifier
            subject: The notification subject type
            channel: The preferred channel for this user/subject
            
        Raises:
            APIError: If the API request fails
            ValidationError: If the request is invalid
            
        Example:
            client.set_preference("user123", "marketing", Channel.PUSH)
        """
        url = self._build_url("/preferences/set")
        payload = {
            "user": user,
            "subject": subject,
            "channel": channel.value
        }
        
        response = self._sync_session.post(
            url,
            json=payload,
            timeout=self.timeout
        )
        
        self._handle_response(response)
        logger.info(f"Set preference for user '{user}', subject '{subject}' to {channel.value}")
    
    def get_preference(self, user: str, subject: str) -> PreferenceResponse:
        """
        Get the effective notification preference for a user and subject.
        
        Falls back to the default channel if no user-specific preference exists.
        
        Args:
            user: The user identifier
            subject: The notification subject type
            
        Returns:
            PreferenceResponse: The preference details including whether it's a default
            
        Raises:
            NotFoundError: If no preference or default exists
            APIError: If the API request fails
            
        Example:
            pref = client.get_preference("user123", "marketing")
            print(f"Channel: {pref.channel}, Is Default: {pref.is_default}")
        """
        url = self._build_url("/preferences/get")
        params = {"user": user, "subject": subject}
        
        response = self._sync_session.get(
            url,
            params=params,
            timeout=self.timeout
        )
        
        data = self._handle_response(response)
        channel = Channel(data) if isinstance(data, str) else Channel(data["channel"])
        
        # Try to determine if this came from user preference or default
        is_default = not self._has_explicit_preference(user, subject)
        
        return PreferenceResponse(
            user=user,
            subject=subject,
            channel=channel,
            is_default=is_default
        )
    
    def _has_explicit_preference(self, user: str, subject: str) -> bool:
        """
        Check if a user has an explicit preference (not a default fallback).
        
        Note: This is a best-effort check that may require additional API support.
        """
        # This is a heuristic - you might need a dedicated endpoint
        try:
            default = self.get_default(subject)
            user_pref = self.get_preference(user, subject)
            return user_pref.channel != default
        except NotFoundError:
            return False
    
    def get_preference_or_none(self, user: str, subject: str) -> Optional[PreferenceResponse]:
        """
        Get preference for a user/subject, returning None if not found.
        
        Args:
            user: The user identifier
            subject: The notification subject type
            
        Returns:
            Optional[PreferenceResponse]: The preference, or None if not found
            
        Example:
            pref = client.get_preference_or_none("user123", "marketing")
            if pref:
                print(f"Using {pref.channel} channel")
        """
        try:
            return self.get_preference(user, subject)
        except NotFoundError:
            return None
    
    def batch_set_defaults(self, defaults: List[Dict[str, str]]) -> Dict[str, Any]:
        """
        Set multiple defaults at once.
        
        Args:
            defaults: List of dicts with 'subject' and 'channel' keys
            
        Returns:
            Dict with success/failure counts
            
        Example:
            results = client.batch_set_defaults([
                {"subject": "marketing", "channel": "email"},
                {"subject": "security", "channel": "push"}
            ])
        """
        results = {"success": 0, "failed": 0, "errors": []}
        
        for item in defaults:
            try:
                self.set_default(item["subject"], Channel(item["channel"]))
                results["success"] += 1
            except Exception as e:
                results["failed"] += 1
                results["errors"].append({
                    "subject": item["subject"],
                    "error": str(e)
                })
        
        return results
    
    def batch_set_preferences(self, preferences: List[BatchPreferenceRequest]) -> Dict[str, Any]:
        """
        Set multiple user preferences at once.
        
        Args:
            preferences: List of BatchPreferenceRequest objects
            
        Returns:
            Dict with success/failure counts
            
        Example:
            results = client.batch_set_preferences([
                BatchPreferenceRequest("user1", "marketing", Channel.EMAIL),
                BatchPreferenceRequest("user1", "security", Channel.PUSH)
            ])
        """
        results = {"success": 0, "failed": 0, "errors": []}
        
        for pref in preferences:
            try:
                self.set_preference(pref.user, pref.subject, pref.channel)
                results["success"] += 1
            except Exception as e:
                results["failed"] += 1
                results["errors"].append({
                    "user": pref.user,
                    "subject": pref.subject,
                    "error": str(e)
                })
        
        return results
    
    def health_check(self) -> bool:
        """
        Check if the service is reachable.
        
        Returns:
            bool: True if the service is healthy
            
        Example:
            if client.health_check():
                print("Service is healthy")
        """
        try:
            url = self._build_url("/defaults/get")
            response = self._sync_session.get(
                url,
                params={"subject": "__health_check__"},
                timeout=5
            )
            # 404 is acceptable - means the service is responding
            return response.status_code in (200, 404)
        except Exception as e:
            logger.warning(f"Health check failed: {e}")
            return False
    
    # =========================================================================
    # Asynchronous Methods
    # =========================================================================
    
    async def _get_async_session(self) -> ClientSession:
        """Get or create an async HTTP session."""
        if self._async_session is None or self._async_session.closed:
            timeout = ClientTimeout(total=self.timeout)
            self._async_session = ClientSession(
                timeout=timeout,
                headers=self.default_headers
            )
        return self._async_session
    
    async def async_set_default(self, subject: str, channel: Channel) -> None:
        """
        Set a default notification channel for a subject (async).
        
        Args:
            subject: The notification subject type
            channel: The default channel to use
            
        Example:
            await client.async_set_default("marketing", Channel.EMAIL)
        """
        session = await self._get_async_session()
        url = self._build_url("/defaults/set")
        payload = {"subject": subject, "channel": channel.value}
        
        async with session.post(
            url,
            json=payload,
            ssl=self.verify_ssl
        ) as response:
            await self._async_handle_response(response)
        
        logger.info(f"Async set default channel for subject '{subject}' to {channel.value}")
    
    async def async_get_default(self, subject: str) -> Channel:
        """
        Get the default notification channel for a subject (async).
        
        Args:
            subject: The notification subject type
            
        Returns:
            Channel: The default channel for the subject
            
        Example:
            channel = await client.async_get_default("marketing")
        """
        session = await self._get_async_session()
        url = self._build_url("/defaults/get")
        params = {"subject": subject}
        
        async with session.get(
            url,
            params=params,
            ssl=self.verify_ssl
        ) as response:
            data = await self._async_handle_response(response)
        
        return Channel(data) if isinstance(data, str) else Channel(data["channel"])
    
    async def async_set_preference(self, user: str, subject: str, channel: Channel) -> None:
        """
        Set a user-specific notification preference (async).
        
        Args:
            user: The user identifier
            subject: The notification subject type
            channel: The preferred channel for this user/subject
            
        Example:
            await client.async_set_preference("user123", "marketing", Channel.PUSH)
        """
        session = await self._get_async_session()
        url = self._build_url("/preferences/set")
        payload = {"user": user, "subject": subject, "channel": channel.value}
        
        async with session.post(
            url,
            json=payload,
            ssl=self.verify_ssl
        ) as response:
            await self._async_handle_response(response)
        
        logger.info(f"Async set preference for user '{user}', subject '{subject}' to {channel.value}")
    
    async def async_get_preference(self, user: str, subject: str) -> PreferenceResponse:
        """
        Get the effective notification preference (async).
        
        Args:
            user: The user identifier
            subject: The notification subject type
            
        Returns:
            PreferenceResponse: The preference details
            
        Example:
            pref = await client.async_get_preference("user123", "marketing")
        """
        session = await self._get_async_session()
        url = self._build_url("/preferences/get")
        params = {"user": user, "subject": subject}
        
        async with session.get(
            url,
            params=params,
            ssl=self.verify_ssl
        ) as response:
            data = await self._async_handle_response(response)
        
        channel = Channel(data) if isinstance(data, str) else Channel(data["channel"])
        is_default = not await self._async_has_explicit_preference(user, subject)
        
        return PreferenceResponse(
            user=user,
            subject=subject,
            channel=channel,
            is_default=is_default
        )
    
    async def _async_has_explicit_preference(self, user: str, subject: str) -> bool:
        """Async version of explicit preference check."""
        try:
            default = await self.async_get_default(subject)
            user_pref = await self.async_get_preference(user, subject)
            return user_pref.channel != default
        except NotFoundError:
            return False
    
    async def async_batch_set_preferences(
        self, preferences: List[BatchPreferenceRequest]
    ) -> Dict[str, Any]:
        """
        Set multiple user preferences at once (async).
        
        Args:
            preferences: List of BatchPreferenceRequest objects
            
        Returns:
            Dict with success/failure counts
            
        Example:
            results = await client.async_batch_set_preferences([
                BatchPreferenceRequest("user1", "marketing", Channel.EMAIL)
            ])
        """
        results = {"success": 0, "failed": 0, "errors": []}
        
        for pref in preferences:
            try:
                await self.async_set_preference(pref.user, pref.subject, pref.channel)
                results["success"] += 1
            except Exception as e:
                results["failed"] += 1
                results["errors"].append({
                    "user": pref.user,
                    "subject": pref.subject,
                    "error": str(e)
                })
        
        return results
    
    async def async_close(self):
        """Close the async session."""
        if self._async_session and not self._async_session.closed:
            await self._async_session.close()
    
    # =========================================================================
    # Context Manager Support
    # =========================================================================
    
    def close(self):
        """Close the synchronous session."""
        if self._sync_session:
            self._sync_session.close()
    
    def __enter__(self):
        """Context manager entry for synchronous usage."""
        return self
    
    def __exit__(self, exc_type, exc_val, exc_tb):
        """Context manager exit for synchronous usage."""
        self.close()
    
    async def __aenter__(self):
        """Context manager entry for async usage."""
        return self
    
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Context manager exit for async usage."""
        await self.async_close()
    
    def __repr__(self) -> str:
        return f"NotificationPrefsClient(base_url='{self.base_url}', api_version='{self.api_version}')"


# =============================================================================
# Usage Examples
# =============================================================================

if __name__ == "__main__":
    # Example 1: Synchronous usage
    print("=== Synchronous Example ===")
    with NotificationPrefsClient("http://localhost:8080/api", api_version="v1") as client:
        # Set defaults
        client.set_default("marketing", Channel.EMAIL)
        client.set_default("security", Channel.PUSH)
        client.set_default("updates", Channel.IN_APP)
        
        # Get defaults
        marketing_channel = client.get_default("marketing")
        print(f"Default marketing channel: {marketing_channel}")
        
        # Set user preferences
        client.set_preference("user123", "marketing", Channel.PUSH)
        client.set_preference("user123", "security", Channel.EMAIL)
        
        # Get effective preferences (with fallback)
        pref = client.get_preference("user123", "marketing")
        print(f"User preference: {pref}")
        
        pref = client.get_preference("user123", "updates")  # Uses default
        print(f"User preference (default fallback): {pref}")
        
        # Batch operations
        results = client.batch_set_preferences([
            BatchPreferenceRequest("user456", "marketing", Channel.SMS),
            BatchPreferenceRequest("user456", "security", Channel.PUSH),
        ])
        print(f"Batch results: {results}")
    
    # Example 2: Asynchronous usage
    async def async_example():
        print("\n=== Async Example ===")
        async with NotificationPrefsClient("http://localhost:8080") as client:
            # Set preferences
            await client.async_set_preference("user789", "marketing", Channel.EMAIL)
            
            # Get preference
            pref = await client.async_get_preference("user789", "marketing")
            print(f"Async user preference: {pref}")
            
            # Batch operations
            results = await client.async_batch_set_preferences([
                BatchPreferenceRequest("user101", "marketing", Channel.PUSH),
                BatchPreferenceRequest("user101", "updates", Channel.IN_APP),
            ])
            print(f"Async batch results: {results}")
    
    # Uncomment to run async example:
    # import asyncio
    # asyncio.run(async_example())
