use alloy::sol;

sol! {
    #[sol(rpc)]
    contract MockToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function transfer(address to, uint256 amount) external returns (bool);
        function balanceOf(address owner) external view returns (uint256);
    }

    #[sol(rpc)]
    contract Vault {
        function matchOrders(address a, address b, uint256 amountA, uint256 amountB) external;
        function withdraw(uint256 amount) external;
        function balanceOf(address user) external view returns (uint256);
    }
}
