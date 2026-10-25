// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// Test tokens for the system tests: worthless, local Anvil chains only. They differ the
// way real tokens do, because the Wallet pays gas differently for each:
//   Token(permit=true,  authorization=true)  like USDC: EIP-2612 permit + EIP-3009
//   Token(permit=true,  authorization=false) EIP-2612 permit only
//   Token(permit=false, authorization=false) a plain ERC-20
//   TetherToken                              like USDT on Ethereum: no return values
// A feature that is off reverts, so the Wallet's probes find it unsupported.

contract Token {
    string public name;
    string public symbol;
    uint8 public immutable decimals;
    bool private immutable permits;
    bool private immutable authorizes;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    mapping(address => uint256) private permitNonces;
    mapping(address => mapping(bytes32 => bool)) private usedAuthorizations;
    bytes32 private immutable separator;

    bytes32 public constant PERMIT_TYPEHASH =
        keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)");
    bytes32 private constant TRANSFER_WITH_AUTHORIZATION_TYPEHASH = keccak256(
        "TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)"
    );

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event AuthorizationUsed(address indexed authorizer, bytes32 indexed nonce);

    constructor(string memory name_, string memory symbol_, uint8 decimals_, uint256 supply, bool permit_, bool authorization_) {
        name = name_;
        symbol = symbol_;
        decimals = decimals_;
        permits = permit_;
        authorizes = authorization_;
        separator = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes(name_)),
                keccak256(bytes("1")),
                block.chainid,
                address(this)
            )
        );
        totalSupply = supply;
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function version() external view returns (string memory) {
        require(permits || authorizes, "no signatures");
        return "1";
    }

    function DOMAIN_SEPARATOR() external view returns (bytes32) {
        require(permits || authorizes, "no signatures");
        return separator;
    }

    function nonces(address owner) external view returns (uint256) {
        require(permits, "no permit");
        return permitNonces[owner];
    }

    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool) {
        require(authorizes, "no authorization");
        return usedAuthorizations[authorizer][nonce];
    }

    function transfer(address to, uint256 value) external returns (bool) {
        _transfer(msg.sender, to, value);
        return true;
    }

    function approve(address spender, uint256 value) external returns (bool) {
        _approve(msg.sender, spender, value);
        return true;
    }

    function transferFrom(address from, address to, uint256 value) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= value, "allowance");
        if (allowed != type(uint256).max) _approve(from, msg.sender, allowed - value);
        _transfer(from, to, value);
        return true;
    }

    function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s)
        external
    {
        require(permits, "no permit");
        require(block.timestamp <= deadline, "permit expired");
        bytes32 digest = keccak256(
            abi.encode(PERMIT_TYPEHASH, owner, spender, value, permitNonces[owner]++, deadline)
        );
        require(_recover(digest, v, r, s) == owner, "invalid permit");
        _approve(owner, spender, value);
    }

    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        require(authorizes, "no authorization");
        require(block.timestamp > validAfter && block.timestamp < validBefore, "authorization not valid now");
        require(!usedAuthorizations[from][nonce], "authorization used");
        bytes32 digest = keccak256(
            abi.encode(TRANSFER_WITH_AUTHORIZATION_TYPEHASH, from, to, value, validAfter, validBefore, nonce)
        );
        require(_recover(digest, v, r, s) == from, "invalid authorization");
        usedAuthorizations[from][nonce] = true;
        emit AuthorizationUsed(from, nonce);
        _transfer(from, to, value);
    }

    function _recover(bytes32 structHash, uint8 v, bytes32 r, bytes32 s) private view returns (address) {
        address signer = ecrecover(keccak256(abi.encodePacked("\x19\x01", separator, structHash)), v, r, s);
        require(signer != address(0), "invalid signature");
        return signer;
    }

    function _approve(address owner, address spender, uint256 value) private {
        allowance[owner][spender] = value;
        emit Approval(owner, spender, value);
    }

    function _transfer(address from, address to, uint256 value) private {
        require(to != address(0), "zero recipient");
        require(balanceOf[from] >= value, "balance");
        balanceOf[from] -= value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
    }
}

/// Like Tether's Ethereum USDT: `transfer`, `transferFrom` and `approve` return nothing.
contract TetherToken {
    string public name;
    string public symbol;
    uint8 public immutable decimals;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(string memory name_, string memory symbol_, uint8 decimals_, uint256 supply) {
        name = name_;
        symbol = symbol_;
        decimals = decimals_;
        totalSupply = supply;
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function transfer(address to, uint256 value) external {
        _transfer(msg.sender, to, value);
    }

    function approve(address spender, uint256 value) external {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
    }

    function transferFrom(address from, address to, uint256 value) external {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= value, "allowance");
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - value;
        _transfer(from, to, value);
    }

    function _transfer(address from, address to, uint256 value) private {
        require(balanceOf[from] >= value, "balance");
        balanceOf[from] -= value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
    }
}
